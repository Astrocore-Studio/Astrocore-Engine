use std::collections::{BTreeMap, BTreeSet, VecDeque};

use astrocore_core::{NetworkEntityId, ObjectId, Revision};
use astrocore_math::glam::Vec3;
use astrocore_math::{ChunkCoord, GridCell};
use astrocore_physics::{BoxWorld, CharacterShape, CharacterState};
use astrocore_protocol::{
    ClientMessage, ProtocolError, RejectReason, ServerMessage, decode, encode,
};
use astrocore_runtime::doors::{
    DEFAULT_REACH_M, DoorEvent, DoorRegistry, InteractError, InteractRequest,
};
use astrocore_runtime::modules::{HookEvent, ModuleHost};
use astrocore_spatial::{Channel, Topology};
use astrocore_terrain::{Terrain, TerrainEdit, TerrainOp};
use astrocore_world::{FloorId, World, WorldCommand, WorldError, WorldOp};

use crate::movement::{InputCmd, apply_input};
use crate::snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// Результат работы ядра для транспорта.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Send { to: ConnId, msg: ServerMessage },
    Disconnect(ConnId),
}

/// Личность, которую возвращает проверка при входе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    /// Метки доступа (см. `DoorState::required_access`).
    pub access: BTreeSet<String>,
    /// Может ли игрок править структуру мира.
    pub can_edit: bool,
}

/// Узел проверки подключения. Конкретные правила (аккаунты, роли) задаёт игра.
pub trait Authenticator: Send {
    fn authenticate(&mut self, build: &str) -> Result<Identity, RejectReason>;
}

/// Пускает всех; права на правки задаются при создании. Для разработки и тестов.
pub struct AllowAll {
    pub can_edit: bool,
}

impl Authenticator for AllowAll {
    fn authenticate(&mut self, build: &str) -> Result<Identity, RejectReason> {
        Ok(Identity {
            name: format!("player-{build}"),
            access: BTreeSet::new(),
            can_edit: self.can_edit,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ServerNetConfig {
    pub tick_hz: u8,
    /// Радиус зоны интереса в чанках (Чебышёв).
    pub interest_radius_chunks: i32,
    /// Максимум обновлений сущностей клиенту за тик.
    pub entity_budget: usize,
    pub max_clients: usize,
    /// Через сколько тиков без `Hello` соединение закрывается.
    pub handshake_timeout_ticks: u32,
    pub input_queue_max: usize,
    /// Ёмкость «ведра» сообщений и его пополнение за тик.
    pub rate_capacity: f32,
    pub rate_refill_per_tick: f32,
    /// Сколько нарушений терпим до отключения.
    pub max_strikes: u8,
    pub reach_m: f32,
    pub spawn: Vec3,
    pub shape: CharacterShape,
    /// Сколько байт содержимого отправляется клиенту за тик: игровой поток не должен голодать.
    pub content_bytes_per_tick: usize,
    /// Сколько запросов содержимого может ждать у одного клиента.
    pub content_queue_max: usize,
}

impl Default for ServerNetConfig {
    fn default() -> Self {
        Self {
            tick_hz: 20,
            interest_radius_chunks: 2,
            entity_budget: 32,
            max_clients: 64,
            handshake_timeout_ticks: 200,
            input_queue_max: 32,
            rate_capacity: 60.0,
            rate_refill_per_tick: 3.0,
            max_strikes: 20,
            reach_m: DEFAULT_REACH_M,
            spawn: Vec3::new(0.5, 1.0, 0.5),
            shape: CharacterShape::default(),
            content_bytes_per_tick: 64 * 1024,
            content_queue_max: 8,
        }
    }
}

struct Player {
    net: NetworkEntityId,
    identity: Identity,
    state: CharacterState,
    yaw: f32,
    queue: VecDeque<InputCmd>,
    last_seq: u32,
    bucket: f32,
    strikes: u8,
    /// Что клиент уже знает о других сущностях: id → тик последней отправки.
    known: BTreeMap<NetworkEntityId, u32>,
    /// Тик, когда состояние последний раз менялось.
    moved_tick: u32,
    /// Динамические объекты (двери, платформы), которых игрок касается сейчас.
    touching: BTreeSet<ObjectId>,
}

/// Счётчики наблюдаемости (раздел 21): экспортируются в лог/метрики хозяином процесса.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerMetrics {
    pub ticks: u64,
    pub packets_in: u64,
    pub messages_out: u64,
    pub rejected_packets: u64,
    pub edits_applied: u64,
    pub inputs_applied: u64,
    pub door_toggles: u64,
    /// Длительность последнего и самого долгого тика, мкс.
    pub tick_micros_last: u64,
    pub tick_micros_max: u64,
}

impl ServerMetrics {
    /// Текстовый экспорт (формат `имя значение`, по строке на метрику) — для логов и сборщиков.
    pub fn export_text(&self, players: usize, revision: u64, rebuild_queue: usize) -> String {
        let rows: [(&str, u64); 12] = [
            ("ticks", self.ticks),
            ("packets_in", self.packets_in),
            ("messages_out", self.messages_out),
            ("rejected_packets", self.rejected_packets),
            ("edits_applied", self.edits_applied),
            ("inputs_applied", self.inputs_applied),
            ("door_toggles", self.door_toggles),
            ("tick_micros_last", self.tick_micros_last),
            ("tick_micros_max", self.tick_micros_max),
            ("players", players as u64),
            ("world_revision", revision),
            ("rebuild_queue", rebuild_queue as u64),
        ];
        rows.iter()
            .map(|(k, v)| format!("astrocore_{k} {v}\n"))
            .collect()
    }
}

pub struct GameServer {
    terrain: Option<Terrain>,
    applied_terrain_ops: Vec<TerrainOp>,
    metrics: ServerMetrics,
    applied_ops: Vec<WorldOp>,
    world: World,
    physics: BoxWorld,
    doors: DoorRegistry,
    door_net: BTreeMap<ObjectId, NetworkEntityId>,
    door_by_net: BTreeMap<NetworkEntityId, ObjectId>,
    cfg: ServerNetConfig,
    auth: Box<dyn Authenticator>,
    modules: Option<ModuleHost>,
    replication: astrocore_api::ReplicationRegistry,
    content: BTreeMap<[u8; 32], std::sync::Arc<Vec<u8>>>,
    content_queue: BTreeMap<ConnId, VecDeque<([u8; 32], u32)>>,
    /// Последние значения реплицируемых данных: (сущность, вид) → (номер, байты).
    components: BTreeMap<(NetworkEntityId, String), (u32, Vec<u8>)>,
    /// Соединения до `Hello`: тик подключения.
    pending: BTreeMap<ConnId, u32>,
    players: BTreeMap<ConnId, Player>,
    next_net: u32,
    tick: u32,
    topo_cache: BTreeMap<FloorId, (Revision, Topology)>,
}

impl GameServer {
    pub fn new(world: World, cfg: ServerNetConfig, auth: Box<dyn Authenticator>) -> Self {
        let mut s = Self {
            terrain: None,
            applied_terrain_ops: Vec::new(),
            metrics: ServerMetrics::default(),
            applied_ops: Vec::new(),
            physics: BoxWorld::from_world(&world),
            doors: DoorRegistry::new(),
            door_net: BTreeMap::new(),
            door_by_net: BTreeMap::new(),
            world,
            cfg,
            auth,
            modules: None,
            replication: astrocore_api::ReplicationRegistry::new(),
            content: BTreeMap::new(),
            content_queue: BTreeMap::new(),
            components: BTreeMap::new(),
            pending: BTreeMap::new(),
            players: BTreeMap::new(),
            next_net: 1,
            tick: 0,
            topo_cache: BTreeMap::new(),
        };
        s.sync_doors();
        s
    }

    /// Рельеф мира: игроки стоят на земле, правки реплицируются.
    pub fn with_terrain(mut self, terrain: Terrain) -> Self {
        self.terrain = Some(terrain);
        self
    }

    pub fn terrain(&self) -> Option<&Terrain> {
        self.terrain.as_ref()
    }

    /// Операции над рельефом с прошлого вызова (для журнала сохранений).
    pub fn take_applied_terrain_ops(&mut self) -> Vec<TerrainOp> {
        std::mem::take(&mut self.applied_terrain_ops)
    }

    /// Серверная правка рельефа без проверки прав (скрипты, игровые модули).
    pub fn apply_terrain_edit(&mut self, edit: &TerrainEdit) -> Result<Vec<Out>, String> {
        let terrain = self.terrain.as_mut().ok_or("рельефа нет")?;
        let op = terrain.apply(edit).map_err(|e| e.to_string())?;
        self.applied_terrain_ops.push(op.clone());
        let mut out = Vec::new();
        if let Ok(data) = postcard::to_stdvec(&op.edit) {
            self.broadcast(&mut out, ServerMessage::TerrainOp { seq: op.seq, data });
        }
        Ok(out)
    }

    /// Виды данных, которые игровые модули могут привязывать к сущностям и реплицировать.
    pub fn with_replication(mut self, registry: astrocore_api::ReplicationRegistry) -> Self {
        self.replication = registry;
        self
    }

    /// Задать значение зарегистрированного вида данных для сущности и разослать его клиентам
    /// по правилам вида (канал, зона интереса). Неизвестный вид, слишком большое значение или
    /// несуществующая сущность — ошибка, состояние не меняется.
    pub fn set_component(
        &mut self,
        net: NetworkEntityId,
        kind: &str,
        data: Vec<u8>,
    ) -> Result<Vec<Out>, String> {
        let spec = self
            .replication
            .check(kind, data.len())
            .map_err(|e| e.to_string())?
            .clone();
        let Some(owner) = self.players.values().find(|p| p.net == net) else {
            return Err(format!("сущность {net:?} не найдена"));
        };
        let owner_pos = owner.state.pos;
        let key = (net, kind.to_string());
        let seq = self
            .components
            .get(&key)
            .map_or(1, |(s, _)| s.wrapping_add(1));
        self.components.insert(key, (seq, data.clone()));
        let msg = ServerMessage::ComponentDelta {
            net,
            kind: kind.to_string(),
            seq,
            reliable: spec.channel == astrocore_api::ReplicationChannel::ReliableOrdered,
            data,
        };
        let out = self
            .players
            .iter()
            .filter(|(_, p)| match spec.interest {
                astrocore_api::Interest::Everyone => true,
                astrocore_api::Interest::Radius(r) => {
                    p.net == net || p.state.pos.distance(owner_pos) <= r
                }
            })
            .map(|(&to, _)| Out::Send {
                to,
                msg: msg.clone(),
            })
            .collect();
        Ok(out)
    }

    /// Сколько частей содержимого ждёт отправки клиенту (диагностика и тесты).
    pub fn content_queue_len(&self, conn: ConnId) -> usize {
        self.content_queue.get(&conn).map_or(0, VecDeque::len)
    }

    /// Сделать содержимое доступным клиентам по хешу (SHA-256). Возвращает хеш.
    pub fn register_content(&mut self, bytes: Vec<u8>) -> Result<[u8; 32], String> {
        use sha2::{Digest, Sha256};
        let parts = bytes.len().div_ceil(astrocore_protocol::CONTENT_PART_BYTES);
        if bytes.is_empty() || parts as u32 > astrocore_protocol::MAX_CONTENT_PARTS {
            return Err("содержимое пусто или превышает 64 МиБ".into());
        }
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        self.content.insert(hash, std::sync::Arc::new(bytes));
        Ok(hash)
    }

    fn on_request_content(&mut self, conn: ConnId, hash: [u8; 32], out: &mut Vec<Out>) {
        let Some(blob) = self.content.get(&hash) else {
            out.push(Out::Send {
                to: conn,
                msg: ServerMessage::ContentMissing { hash },
            });
            return;
        };
        let total = blob.len().div_ceil(astrocore_protocol::CONTENT_PART_BYTES) as u32;
        let queue = self.content_queue.entry(conn).or_default();
        let waiting = queue.iter().filter(|(h, i)| *i == 0 && *h != hash).count();
        if waiting >= self.cfg.content_queue_max || queue.iter().any(|(h, _)| *h == hash) {
            return; // слишком много запросов или такой уже в очереди
        }
        queue.extend((0..total).map(|i| (hash, i)));
    }

    /// Отправить часть очередей содержимого в пределах бюджета тика.
    fn pump_content(&mut self, out: &mut Vec<Out>) {
        let part = astrocore_protocol::CONTENT_PART_BYTES;
        let conns: Vec<ConnId> = self.content_queue.keys().copied().collect();
        for conn in conns {
            let mut budget = self.cfg.content_bytes_per_tick;
            while budget > 0 {
                let Some(queue) = self.content_queue.get_mut(&conn) else {
                    break;
                };
                let Some((hash, index)) = queue.pop_front() else {
                    break;
                };
                let Some(blob) = self.content.get(&hash) else {
                    continue;
                };
                let total = blob.len().div_ceil(part) as u32;
                let start = index as usize * part;
                let end = (start + part).min(blob.len());
                out.push(Out::Send {
                    to: conn,
                    msg: ServerMessage::ContentPart {
                        hash,
                        index,
                        total,
                        data: blob[start..end].to_vec(),
                    },
                });
                budget = budget.saturating_sub(end - start);
            }
            if self
                .content_queue
                .get(&conn)
                .is_some_and(VecDeque::is_empty)
            {
                self.content_queue.remove(&conn);
            }
        }
    }

    pub fn with_modules(mut self, host: ModuleHost) -> Self {
        self.modules = Some(host);
        self
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn doors(&self) -> &DoorRegistry {
        &self.doors
    }

    pub fn metrics(&self) -> ServerMetrics {
        self.metrics
    }

    /// Метрики текстом; очередь пересборки у сервера пуста — коллизия и топология строятся
    /// синхронно по ревизиям.
    pub fn metrics_text(&self) -> String {
        self.metrics
            .export_text(self.players.len(), self.world.revision().0, 0)
    }

    /// Операции, применённые к миру с прошлого вызова (для журнала сохранений).
    pub fn take_applied_ops(&mut self) -> Vec<WorldOp> {
        std::mem::take(&mut self.applied_ops)
    }

    pub fn tick_number(&self) -> u32 {
        self.tick
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    pub fn player_position(&self, conn: ConnId) -> Option<Vec3> {
        self.players.get(&conn).map(|p| p.state.pos)
    }

    /// Изменить состояние двери напрямую (игровая логика: замки, электроника).
    pub fn door_state_mut(
        &mut self,
        element: ObjectId,
    ) -> Option<&mut astrocore_runtime::doors::DoorState> {
        self.doors.get_mut(element)
    }

    fn alloc_net(&mut self) -> NetworkEntityId {
        let id = NetworkEntityId(self.next_net);
        self.next_net += 1;
        id
    }

    fn sync_doors(&mut self) {
        self.doors.sync(&self.world);
        let present: BTreeSet<ObjectId> = self
            .world
            .elements()
            .filter(|e| self.doors.get(e.id).is_some())
            .map(|e| e.id)
            .collect();
        for id in &present {
            if !self.door_net.contains_key(id) {
                let net = self.alloc_net();
                self.door_net.insert(*id, net);
                self.door_by_net.insert(net, *id);
            }
        }
        let gone: Vec<ObjectId> = self
            .door_net
            .keys()
            .filter(|id| !present.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(net) = self.door_net.remove(&id) {
                self.door_by_net.remove(&net);
            }
        }
        let doors = &self.doors;
        self.physics.sync_doors(&self.world, |id| doors.is_open(id));
    }

    // ---- соединения ---------------------------------------------------------

    pub fn connect(&mut self, conn: ConnId) -> Vec<Out> {
        if self.players.len() + self.pending.len() >= self.cfg.max_clients {
            return vec![
                Out::Send {
                    to: conn,
                    msg: reject(RejectReason::ServerFull, "сервер переполнен"),
                },
                Out::Disconnect(conn),
            ];
        }
        self.pending.insert(conn, self.tick);
        Vec::new()
    }

    pub fn disconnect(&mut self, conn: ConnId) -> Vec<Out> {
        self.pending.remove(&conn);
        let Some(p) = self.players.remove(&conn) else {
            return Vec::new();
        };
        self.components.retain(|(n, _), _| *n != p.net);
        self.content_queue.remove(&conn);
        let mut out: Vec<Out> = self
            .players
            .keys()
            .map(|&to| Out::Send {
                to,
                msg: ServerMessage::EntityRemoved { id: p.net },
            })
            .collect();
        self.dispatch_hook(
            HookEvent::PlayerLeft {
                actor: p.net,
                pos: p.state.pos,
            },
            &mut out,
        );
        out
    }

    // ---- входящие пакеты ----------------------------------------------------

    pub fn on_packet(&mut self, conn: ConnId, packet: &[u8]) -> Vec<Out> {
        let _span = tracing::trace_span!("server_packet", conn = conn.0).entered();
        self.metrics.packets_in += 1;
        let out = self.handle_packet(conn, packet);
        self.metrics.messages_out += out.len() as u64;
        self.metrics.rejected_packets += out
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    Out::Send {
                        msg: ServerMessage::Reject { .. },
                        ..
                    }
                )
            })
            .count() as u64;
        out
    }

    fn handle_packet(&mut self, conn: ConnId, packet: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        // Лимит частоты применяется ко всем соединениям, включая ещё не вошедшие.
        if !self.take_token(conn) {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::RateLimited, "слишком часто"),
            });
            self.strike(conn, &mut out);
            return out;
        }
        let msg: ClientMessage = match decode(packet) {
            Ok(m) => m,
            Err(ProtocolError::VersionMismatch { got }) => {
                out.push(Out::Send {
                    to: conn,
                    msg: reject(
                        RejectReason::IncompatibleProtocol,
                        &format!("версия протокола {got}"),
                    ),
                });
                out.push(Out::Disconnect(conn));
                return out;
            }
            Err(e) => {
                out.push(Out::Send {
                    to: conn,
                    msg: reject(RejectReason::InvalidCommand, &e.to_string()),
                });
                self.strike(conn, &mut out);
                return out;
            }
        };
        match msg {
            ClientMessage::Hello {
                build,
                world_id,
                world_hash,
            } => self.on_hello(conn, &build, &world_id, world_hash, &mut out),
            other => {
                if !self.players.contains_key(&conn) {
                    out.push(Out::Send {
                        to: conn,
                        msg: reject(RejectReason::NotAuthorized, "сначала Hello"),
                    });
                    self.strike(conn, &mut out);
                    return out;
                }
                match other {
                    ClientMessage::Hello { .. } => {}
                    ClientMessage::Input {
                        seq,
                        move_x,
                        move_z,
                        yaw,
                    } => self.on_input(
                        conn,
                        InputCmd {
                            seq,
                            move_x,
                            move_z,
                            yaw,
                        },
                    ),
                    ClientMessage::Interact { target } => self.on_interact(conn, target, &mut out),
                    ClientMessage::Edit {
                        base_revision,
                        command,
                    } => self.on_edit(conn, base_revision, command, &mut out),
                    ClientMessage::RequestSnapshot => self.send_snapshot(conn, &mut out),
                    ClientMessage::GameCommand { kind, data } => {
                        if let Some(net) = self.players.get(&conn).map(|p| p.net) {
                            self.dispatch_hook(
                                HookEvent::Command {
                                    actor: net,
                                    kind,
                                    data,
                                },
                                &mut out,
                            );
                        }
                    }
                    ClientMessage::RequestContent { hash } => {
                        self.on_request_content(conn, hash, &mut out)
                    }
                    ClientMessage::EditTerrain {
                        base_revision,
                        data,
                    } => self.on_edit_terrain(conn, base_revision, &data, &mut out),
                }
            }
        }
        out
    }

    fn take_token(&mut self, conn: ConnId) -> bool {
        match self.players.get_mut(&conn) {
            Some(p) if p.bucket >= 1.0 => {
                p.bucket -= 1.0;
                true
            }
            Some(_) => false,
            // До входа лимит мягкий: только Hello и мусор, а мусор считается нарушением.
            None => true,
        }
    }

    fn strike(&mut self, conn: ConnId, out: &mut Vec<Out>) {
        let limit = self.cfg.max_strikes;
        let over = match self.players.get_mut(&conn) {
            Some(p) => {
                p.strikes = p.strikes.saturating_add(1);
                p.strikes >= limit
            }
            // Не вошедшее соединение, присылающее мусор, отключается сразу.
            None => true,
        };
        if over {
            out.push(Out::Disconnect(conn));
        }
    }

    fn on_hello(
        &mut self,
        conn: ConnId,
        build: &str,
        world_id: &str,
        world_hash: u64,
        out: &mut Vec<Out>,
    ) {
        if self.players.contains_key(&conn) {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::InvalidCommand, "повторный Hello"),
            });
            self.strike(conn, out);
            return;
        }
        let fail = |out: &mut Vec<Out>, reason, detail: &str| {
            out.push(Out::Send {
                to: conn,
                msg: reject(reason, detail),
            });
            out.push(Out::Disconnect(conn));
        };
        if world_id != self.world.id() {
            return fail(out, RejectReason::IncompatibleWorld, "другой мир");
        }
        // Нулевой хеш означает «локальной копии карты нет — пришлите снимок».
        if world_hash != 0 && world_hash != self.world.content_hash() {
            return fail(
                out,
                RejectReason::IncompatibleWorld,
                "версия карты не совпадает",
            );
        }
        let identity = match self.auth.authenticate(build) {
            Ok(i) => i,
            Err(reason) => return fail(out, reason, "вход отклонён"),
        };
        self.pending.remove(&conn);
        let net = self.alloc_net();
        let player = Player {
            net,
            identity,
            state: CharacterState::at(self.cfg.spawn),
            yaw: 0.0,
            queue: VecDeque::new(),
            last_seq: 0,
            bucket: self.cfg.rate_capacity,
            strikes: 0,
            known: BTreeMap::new(),
            moved_tick: self.tick,
            touching: BTreeSet::new(),
        };
        self.players.insert(conn, player);
        self.dispatch_hook(
            HookEvent::EntitySpawned {
                id: ObjectId::from_u128(u128::from(net.0)),
            },
            out,
        );
        out.push(Out::Send {
            to: conn,
            msg: ServerMessage::Welcome {
                tick_rate_hz: self.cfg.tick_hz,
                revision: self.world.revision().0,
                you: net,
            },
        });
        // Начальная позиция: клиент выравнивает предсказание по серверной точке появления.
        let spawn = self.cfg.spawn.to_array();
        out.push(Out::Send {
            to: conn,
            msg: ServerMessage::InputAck { seq: 0, pos: spawn },
        });
        self.send_snapshot(conn, out);
    }

    fn send_snapshot(&mut self, conn: ConnId, out: &mut Vec<Out>) {
        match snapshot::split(&self.world.to_state()) {
            Ok(parts) => out.extend(parts.into_iter().map(|msg| Out::Send { to: conn, msg })),
            Err(e) => out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::InvalidCommand, &e.to_string()),
            }),
        }
        if let Some(t) = &self.terrain {
            match snapshot::split_terrain(&t.to_state()) {
                Ok(parts) => out.extend(parts.into_iter().map(|msg| Out::Send { to: conn, msg })),
                Err(e) => out.push(Out::Send {
                    to: conn,
                    msg: reject(RejectReason::InvalidCommand, &e.to_string()),
                }),
            }
        }
        for (element, net) in &self.door_net {
            out.push(Out::Send {
                to: conn,
                msg: ServerMessage::DoorState {
                    net: *net,
                    element: *element,
                    open: self.doors.is_open(*element),
                },
            });
        }
        for ((net, kind), (seq, data)) in &self.components {
            out.push(Out::Send {
                to: conn,
                msg: ServerMessage::ComponentDelta {
                    net: *net,
                    kind: kind.clone(),
                    seq: *seq,
                    reliable: true,
                    data: data.clone(),
                },
            });
        }
        if let Some(p) = self.players.get_mut(&conn) {
            p.known.clear(); // после снимка клиент заново получит видимые сущности
        }
    }

    fn on_input(&mut self, conn: ConnId, cmd: InputCmd) {
        let max = self.cfg.input_queue_max;
        if let Some(p) = self.players.get_mut(&conn) {
            // Старые и повторные команды отбрасываются. Если очередь переполнена (клиент шлёт
            // быстрее, чем идут тики), устаревшие команды вытесняются новыми: подтверждение
            // более новой команды отменяет все предыдущие, а клиент согласует позицию.
            if cmd.seq > p.last_seq && p.queue.back().is_none_or(|b| cmd.seq > b.seq) {
                if p.queue.len() >= max {
                    p.queue.pop_front();
                }
                p.queue.push_back(cmd);
            }
        }
    }

    fn on_interact(&mut self, conn: ConnId, target: NetworkEntityId, out: &mut Vec<Out>) {
        let Some(&element) = self.door_by_net.get(&target) else {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::InvalidCommand, "нет такой сущности"),
            });
            return;
        };
        let (actor_pos, access, actor_net) = {
            let p = &self.players[&conn];
            (
                p.state.pos + Vec3::new(0.0, self.cfg.shape.height * 0.5, 0.0),
                p.identity.access.clone(),
                p.net,
            )
        };
        let req = InteractRequest {
            actor_pos,
            target: element,
            access: &access,
            reach_m: self.cfg.reach_m,
        };
        match self.doors.interact(&self.world, &req) {
            Ok(event) => {
                self.metrics.door_toggles += 1;
                let open = matches!(event, DoorEvent::Opened(_));
                let doors = &self.doors;
                self.physics.sync_doors(&self.world, |id| doors.is_open(id));
                self.broadcast(
                    out,
                    ServerMessage::DoorState {
                        net: target,
                        element,
                        open,
                    },
                );
                self.dispatch_hook(HookEvent::PortalChanged { element, open }, out);
                self.dispatch_hook(
                    HookEvent::ComponentChanged {
                        id: element,
                        component: "door.open".into(),
                    },
                    out,
                );
                let actor_id = ObjectId::from_u128(u128::from(actor_net.0));
                self.dispatch_hook(
                    HookEvent::Interact {
                        actor: actor_id,
                        target: element,
                    },
                    out,
                );
            }
            Err(e) => {
                let reason = match e {
                    InteractError::Locked | InteractError::AccessDenied(_) => {
                        RejectReason::NotAuthorized
                    }
                    _ => RejectReason::InvalidCommand,
                };
                out.push(Out::Send {
                    to: conn,
                    msg: reject(reason, &e.to_string()),
                });
            }
        }
    }

    fn on_edit_terrain(
        &mut self,
        conn: ConnId,
        base_revision: u64,
        data: &[u8],
        out: &mut Vec<Out>,
    ) {
        if !self.players[&conn].identity.can_edit {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::NotAuthorized, "правка рельефа запрещена"),
            });
            self.strike(conn, out);
            return;
        }
        let Some(terrain) = self.terrain.as_ref() else {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::InvalidCommand, "в мире нет рельефа"),
            });
            return;
        };
        if terrain.revision() != base_revision {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::StaleRevision, "устаревшая ревизия рельефа"),
            });
            return;
        }
        let edit: TerrainEdit = match postcard::from_bytes(data) {
            Ok(e) => e,
            Err(e) => {
                out.push(Out::Send {
                    to: conn,
                    msg: reject(RejectReason::InvalidCommand, &e.to_string()),
                });
                self.strike(conn, out);
                return;
            }
        };
        match self.apply_terrain_edit(&edit) {
            Ok(outs) => out.extend(outs),
            Err(e) => out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::InvalidCommand, &e),
            }),
        }
    }

    fn on_edit(
        &mut self,
        conn: ConnId,
        base_revision: u64,
        command: WorldCommand,
        out: &mut Vec<Out>,
    ) {
        if !self.players[&conn].identity.can_edit {
            out.push(Out::Send {
                to: conn,
                msg: reject(RejectReason::NotAuthorized, "правка мира запрещена"),
            });
            self.strike(conn, out);
            return;
        }
        match self.world.apply_checked(command, Revision(base_revision)) {
            Ok(op) => {
                self.metrics.edits_applied += 1;
                self.applied_ops.push(op.clone());
                self.after_world_change();
                self.broadcast(out, ServerMessage::WorldOp(op));
                let new_doors: Vec<(ObjectId, NetworkEntityId)> =
                    self.door_net.iter().map(|(a, b)| (*a, *b)).collect();
                for (element, net) in new_doors {
                    // Состояние новых дверей сообщаем всем (идемпотентно для старых).
                    let open = self.doors.is_open(element);
                    self.broadcast(out, ServerMessage::DoorState { net, element, open });
                }
            }
            Err(e) => {
                let reason = match e {
                    WorldError::StaleRevision { .. } => RejectReason::StaleRevision,
                    _ => RejectReason::InvalidCommand,
                };
                out.push(Out::Send {
                    to: conn,
                    msg: reject(reason, &e.to_string()),
                });
            }
        }
    }

    /// Применить правку к серверной физике и дверям. Запускается и после правок «изнутри»
    /// (игровые модули, [`GameServer::apply_world_command`]).
    fn after_world_change(&mut self) {
        let dirty = self.world.take_dirty();
        self.physics.rebuild_regions(&self.world, &dirty);
        self.sync_doors();
    }

    /// Серверная правка мира без проверки прав (разрушение, скрипты, игровые модули).
    pub fn apply_world_command(&mut self, command: WorldCommand) -> Result<Vec<Out>, WorldError> {
        let _span = tracing::debug_span!("world_revision").entered();
        let op = self.world.apply(command)?;
        self.applied_ops.push(op.clone());
        self.after_world_change();
        let mut out = Vec::new();
        self.broadcast(&mut out, ServerMessage::WorldOp(op));
        Ok(out)
    }

    /// Сообщить модулям, что чанк загружен (вызывает хозяин процесса при стриминге).
    pub fn notify_chunk_loaded(&mut self, chunk: astrocore_math::ChunkCoord) -> Vec<Out> {
        let mut out = Vec::new();
        self.dispatch_hook(HookEvent::ChunkLoaded { chunk }, &mut out);
        out
    }

    fn dispatch_hook(&mut self, event: HookEvent, out: &mut Vec<Out>) {
        let Some(host) = self.modules.as_mut() else {
            return;
        };
        let positions: BTreeMap<NetworkEntityId, Vec3> = self
            .players
            .values()
            .map(|p| (p.net, p.state.pos))
            .collect();
        let result = host.dispatch_with(&self.world, &positions, &event);
        for u in result.components {
            // Несуществующая сущность или незарегистрированный вид — ошибка модуля, а не клиента.
            if let Ok(msgs) = self.set_component(u.net, &u.kind, u.data) {
                out.extend(msgs);
            }
        }
        for cmd in result.commands {
            // Модуль не имеет прямого доступа к миру: его команды проходят обычную валидацию.
            if let Ok(op) = self.world.apply(cmd) {
                self.applied_ops.push(op.clone());
                self.after_world_change();
                self.broadcast(out, ServerMessage::WorldOp(op));
            }
        }
    }

    fn broadcast(&self, out: &mut Vec<Out>, msg: ServerMessage) {
        out.extend(self.players.keys().map(|&to| Out::Send {
            to,
            msg: msg.clone(),
        }));
    }

    // ---- тик ----------------------------------------------------------------

    pub fn tick(&mut self) -> Vec<Out> {
        let _span = tracing::debug_span!("network_tick").entered();
        let started = std::time::Instant::now();
        self.tick += 1;
        self.metrics.ticks += 1;
        let mut out = Vec::new();
        let dt = 1.0 / f32::from(self.cfg.tick_hz);

        // Соединения, не приславшие Hello вовремя.
        let timeout = self.cfg.handshake_timeout_ticks;
        let now = self.tick;
        let stale: Vec<ConnId> = self
            .pending
            .iter()
            .filter(|(_, t)| now - **t > timeout)
            .map(|(c, _)| *c)
            .collect();
        for c in stale {
            self.pending.remove(&c);
            out.push(Out::Disconnect(c));
        }

        // Ввод: не более одной команды за тик на игрока.
        let refill = self.cfg.rate_refill_per_tick;
        let cap = self.cfg.rate_capacity;
        let shape = self.cfg.shape;
        let tick = self.tick;
        let mut collisions: Vec<(ObjectId, ObjectId)> = Vec::new();
        for (&conn, p) in &mut self.players {
            p.bucket = (p.bucket + refill).min(cap);
            // Симуляция игрока идёт только по его командам: одна команда — один шаг, точно как
            // у клиента при предсказании. Игрок без ввода не симулируется (не «падает» и не
            // дрейфует между командами), поэтому клиент и сервер не расходятся из-за задержек сети.
            let Some(cmd) = p.queue.pop_front() else {
                continue;
            };
            let before = p.state.pos;
            apply_input(
                &self.physics,
                self.terrain
                    .as_ref()
                    .map(|t| t as &dyn astrocore_physics::GroundProvider),
                &mut p.state,
                &shape,
                &cmd,
                dt,
            );
            p.last_seq = cmd.seq;
            p.yaw = cmd.yaw;
            self.metrics.inputs_applied += 1;
            out.push(Out::Send {
                to: conn,
                msg: ServerMessage::InputAck {
                    seq: cmd.seq,
                    pos: p.state.pos.to_array(),
                },
            });
            if p.state.pos != before {
                p.moved_tick = tick;
            }
            // Касание динамических объектов порождает событие один раз на начало контакта.
            let body = astrocore_math::Aabb::new(
                p.state.pos + Vec3::new(-shape.radius - 0.05, 0.05, -shape.radius - 0.05),
                p.state.pos + Vec3::new(shape.radius + 0.05, shape.height, shape.radius + 0.05),
            );
            let now: BTreeSet<ObjectId> = self
                .physics
                .overlap_masked(&body, astrocore_physics::LAYER_DYNAMIC)
                .into_iter()
                .collect();
            let actor = ObjectId::from_u128(u128::from(p.net.0));
            collisions.extend(now.difference(&p.touching).map(|&other| (actor, other)));
            p.touching = now;
        }
        for (a, b) in collisions {
            self.dispatch_hook(HookEvent::Collision { a, b }, &mut out);
        }

        self.replicate_entities(&mut out);
        self.pump_content(&mut out);
        self.metrics.messages_out += out.len() as u64;
        let micros = started.elapsed().as_micros() as u64;
        self.metrics.tick_micros_last = micros;
        self.metrics.tick_micros_max = self.metrics.tick_micros_max.max(micros);
        out
    }

    fn floor_of(&self, y: f32) -> Option<FloorId> {
        self.world
            .floors()
            .find(|f| y >= f.base_y - 0.5 && y < f.top_y())
            .map(|f| f.id)
    }

    fn topology(&mut self, floor: FloorId) -> Option<&Topology> {
        let rev = self.world.revision();
        let fresh = matches!(self.topo_cache.get(&floor), Some((r, _)) if *r == rev);
        if !fresh {
            let t = Topology::build(&self.world, floor).ok()?;
            self.topo_cache.insert(floor, (rev, t));
        }
        self.topo_cache.get(&floor).map(|(_, t)| t)
    }

    /// Видит ли наблюдатель сущность: этажи и комнаты с учётом закрытых дверей.
    fn sees(&mut self, a: Vec3, b: Vec3) -> bool {
        let (fa, fb) = (self.floor_of(a.y), self.floor_of(b.y));
        match (fa, fb) {
            (Some(x), Some(y)) if x == y => {
                let step = self.world.config().grid_step_m;
                let (ca, cb) = (GridCell::from_world(a, step), GridCell::from_world(b, step));
                let doors = self.doors.clone();
                let Some(topo) = self.topology(x) else {
                    return true;
                };
                match (topo.room_at(ca), topo.room_at(cb)) {
                    (Some(ra), Some(rb)) if ra != rb => topo
                        .reachable(ra, Channel::Sight, |id| doors.is_open(id))
                        .contains(&rb),
                    _ => true,
                }
            }
            // Соседние этажи видны (атриумы, лестницы); дальше — нет.
            (Some(x), Some(y)) => (x.0 - y.0).abs() <= 1,
            // Вне этажей (падение, улица) — не скрываем.
            _ => true,
        }
    }

    fn replicate_entities(&mut self, out: &mut Vec<Out>) {
        let chunk_size = self.world.config().chunk_size_m;
        let radius = self.cfg.interest_radius_chunks;
        let budget = self.cfg.entity_budget;
        let tick = self.tick;
        let snapshot: Vec<(ConnId, NetworkEntityId, Vec3, f32, u32)> = self
            .players
            .iter()
            .map(|(c, p)| (*c, p.net, p.state.pos, p.yaw, p.moved_tick))
            .collect();

        for &(viewer, _, vpos, _, _) in &snapshot {
            let vchunk = ChunkCoord::from_world(vpos, chunk_size);
            // Кандидаты в зоне интереса и видимости.
            let mut visible: Vec<(NetworkEntityId, Vec3, f32, u32, f32)> = Vec::new();
            for &(other, net, pos, yaw, moved) in &snapshot {
                if other == viewer {
                    continue;
                }
                let oc = ChunkCoord::from_world(pos, chunk_size);
                let dist_chunks = (oc.x - vchunk.x).abs().max((oc.z - vchunk.z).abs());
                if dist_chunks <= radius && self.sees(vpos, pos) {
                    visible.push((net, pos, yaw, moved, vpos.distance(pos)));
                }
            }
            let visible_ids: BTreeSet<NetworkEntityId> = visible.iter().map(|v| v.0).collect();
            let Some(player) = self.players.get_mut(&viewer) else {
                continue;
            };

            // Ушедшие из зоны интереса.
            let gone: Vec<NetworkEntityId> = player
                .known
                .keys()
                .filter(|k| !visible_ids.contains(k))
                .copied()
                .collect();
            for id in gone {
                player.known.remove(&id);
                out.push(Out::Send {
                    to: viewer,
                    msg: ServerMessage::EntityRemoved { id },
                });
            }

            // Приоритет: новые и давно не обновлявшиеся — раньше, близкие — раньше.
            let mut due: Vec<(f32, NetworkEntityId, Vec3, f32)> = visible
                .iter()
                .filter(|(net, _, _, moved, _)| match player.known.get(net) {
                    None => true,
                    Some(&sent) => *moved >= sent,
                })
                .map(|(net, pos, yaw, _, dist)| {
                    let age = player
                        .known
                        .get(net)
                        .map_or(1000.0, |&s| (tick - s) as f32 + 1.0);
                    (age / (1.0 + dist), *net, *pos, *yaw)
                })
                .collect();
            due.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            for (_, net, pos, yaw) in due.into_iter().take(budget) {
                player.known.insert(net, tick);
                out.push(Out::Send {
                    to: viewer,
                    msg: ServerMessage::EntityState {
                        id: net,
                        pos: pos.to_array(),
                        yaw,
                        tick,
                    },
                });
            }
        }
        // Игрок, вышедший за пределы мира, не должен «залипнуть» навсегда: падение — не ошибка протокола.
    }
}

fn reject(reason: RejectReason, detail: &str) -> ServerMessage {
    ServerMessage::Reject {
        reason,
        detail: detail
            .chars()
            .take(astrocore_protocol::MAX_REASON_LEN)
            .collect(),
    }
}

/// Кодирует исходящее сообщение для транспорта.
pub fn encode_out(msg: &ServerMessage) -> Option<Vec<u8>> {
    encode(msg).ok()
}
