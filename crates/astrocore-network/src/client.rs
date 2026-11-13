use std::collections::{BTreeMap, VecDeque};

use astrocore_core::{NetworkEntityId, ObjectId};
use astrocore_math::glam::Vec3;
use astrocore_physics::{BoxWorld, CharacterShape, CharacterState};
use astrocore_protocol::{
    ClientMessage, ProtocolError, RejectReason, ServerMessage, decode, encode,
};
use astrocore_terrain::{Terrain, TerrainEdit, TerrainOp};
use astrocore_world::{ReplicaOutcome, World, WorldCommand, WorldError, WorldOp};

use crate::movement::{InputCmd, apply_input};
use crate::snapshot::{Reassembler, SnapshotError};

/// Расхождение позиции, после которого клиент считает, что сервер его поправил.
pub const CORRECTION_EPSILON_M: f32 = 1e-3;
const MAX_BUFFERED_OPS: usize = 1024;
const MAX_PENDING_INPUTS: usize = 256;
const MAX_REMOTE_SAMPLES: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    Joined {
        you: NetworkEntityId,
    },
    SnapshotApplied {
        revision: u64,
    },
    WorldChanged {
        seq: u64,
    },
    /// Реплика потеряла синхронизацию — клиент уже запросил снимок.
    ResyncRequested,
    DoorChanged {
        net: NetworkEntityId,
        open: bool,
    },
    EntityUpdated(NetworkEntityId),
    EntityRemoved(NetworkEntityId),
    /// Пришло новое значение реплицируемого вида данных сущности.
    ComponentChanged {
        net: NetworkEntityId,
        kind: String,
    },
    /// Содержимое по хешу получено и проверено.
    ContentReceived {
        hash: [u8; 32],
    },
    /// Содержимого нет у сервера или оно повреждено.
    ContentFailed {
        hash: [u8; 32],
    },
    /// Рельеф получен целиком или изменился.
    TerrainChanged,
    /// Сервер начал новый раунд: состояние сброшено, нужно отправить `Hello` заново.
    RoundRestarted,
    Rejected {
        reason: RejectReason,
        detail: String,
    },
    /// Сервер поправил предсказанную позицию на `distance` метров.
    Corrected {
        distance: f32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("протокол: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("снимок: {0}")]
    Snapshot(#[from] SnapshotError),
    #[error("мир: {0}")]
    World(#[from] WorldError),
    #[error("неожиданное сообщение: {0}")]
    Unexpected(&'static str),
}

/// Сколько загрузок содержимого клиент собирает одновременно.
const MAX_CONTENT_DOWNLOADS: usize = 8;

#[derive(Debug, Default)]
struct ContentAssembly {
    total: u32,
    parts: BTreeMap<u32, Vec<u8>>,
    bytes: usize,
}

pub struct ClientCore {
    build: String,
    world_id: String,
    known_world_hash: u64,
    shape: CharacterShape,
    tick_hz: u8,
    you: Option<NetworkEntityId>,
    noclip: bool,
    speed: f32,
    replica: Option<World>,
    physics: BoxWorld,
    reassembler: Reassembler,
    buffered_ops: Vec<WorldOp>,
    doors: BTreeMap<NetworkEntityId, (ObjectId, bool)>,
    components: BTreeMap<(NetworkEntityId, String), (u32, Vec<u8>)>,
    content: BTreeMap<[u8; 32], Vec<u8>>,
    content_partial: BTreeMap<[u8; 32], ContentAssembly>,
    state: CharacterState,
    seq: u32,
    pending: VecDeque<InputCmd>,
    history: VecDeque<(u32, CharacterState)>,
    remotes: BTreeMap<NetworkEntityId, VecDeque<(u32, Vec3, f32)>>,
    render_dirty: Vec<astrocore_world::DirtyRegion>,
    snapshot_serial: u32,
    terrain: Option<Terrain>,
    terrain_reasm: Reassembler,
    terrain_dirty: Vec<astrocore_math::ChunkCoord>,
    terrain_serial: u32,
}

impl ClientCore {
    /// `known_world_hash` — хеш локальной копии карты или 0, если её нет.
    pub fn new(build: &str, world_id: &str, known_world_hash: u64) -> Self {
        Self {
            build: build.to_string(),
            world_id: world_id.to_string(),
            known_world_hash,
            shape: CharacterShape::default(),
            tick_hz: 20,
            you: None,
            noclip: false,
            speed: 1.0,
            replica: None,
            physics: BoxWorld::default(),
            reassembler: Reassembler::new(),
            buffered_ops: Vec::new(),
            doors: BTreeMap::new(),
            components: BTreeMap::new(),
            content: BTreeMap::new(),
            content_partial: BTreeMap::new(),
            state: CharacterState::at(Vec3::ZERO),
            seq: 0,
            pending: VecDeque::new(),
            history: VecDeque::new(),
            remotes: BTreeMap::new(),
            render_dirty: Vec::new(),
            snapshot_serial: 0,
            terrain: None,
            terrain_reasm: Reassembler::new(),
            terrain_dirty: Vec::new(),
            terrain_serial: 0,
        }
    }

    /// Новый раунд на сервере: всё, что пришло от прежнего, забыть. Содержимое по хешу остаётся.
    fn restart(&mut self) {
        self.you = None;
        self.replica = None;
        self.known_world_hash = 0;
        self.physics = BoxWorld::default();
        self.reassembler.reset();
        self.buffered_ops.clear();
        self.doors.clear();
        self.components.clear();
        self.state = CharacterState::at(Vec3::ZERO);
        self.seq = 0;
        self.pending.clear();
        self.history.clear();
        self.remotes.clear();
        self.snapshot_serial += 1;
    }

    // ---- исходящие пакеты -----------------------------------------------------

    pub fn hello_packet(&self) -> Vec<u8> {
        encode(&ClientMessage::Hello {
            build: self.build.clone(),
            world_id: self.world_id.clone(),
            world_hash: self.known_world_hash,
        })
        .expect("Hello валиден")
    }

    pub fn interact_packet(&self, target: NetworkEntityId) -> Vec<u8> {
        encode(&ClientMessage::Interact { target }).expect("Interact валиден")
    }

    pub fn edit_packet(&self, command: WorldCommand) -> Option<Vec<u8>> {
        let base = self.replica.as_ref()?.revision().0;
        encode(&ClientMessage::Edit {
            base_revision: base,
            command,
        })
        .ok()
    }

    /// Правка рельефа (нужны права редактора).
    pub fn edit_terrain_packet(&self, edit: &TerrainEdit) -> Option<Vec<u8>> {
        let base = self.terrain.as_ref()?.revision();
        let data = postcard::to_stdvec(edit).ok()?;
        encode(&ClientMessage::EditTerrain {
            base_revision: base,
            data,
        })
        .ok()
    }

    /// Последнее принятое значение вида данных у сущности.
    pub fn component(&self, net: NetworkEntityId, kind: &str) -> Option<&[u8]> {
        self.components
            .get(&(net, kind.to_string()))
            .map(|(_, d)| d.as_slice())
    }

    /// Игровая команда серверу (`kind` и байты определяет игровой модуль).
    pub fn command_packet(&self, kind: &str, data: Vec<u8>) -> Option<Vec<u8>> {
        encode(&ClientMessage::GameCommand {
            kind: kind.to_string(),
            data,
        })
        .ok()
    }

    pub fn request_content_packet(&self, hash: [u8; 32]) -> Vec<u8> {
        encode(&ClientMessage::RequestContent { hash }).unwrap_or_default()
    }

    /// Полученное и проверенное содержимое.
    pub fn content(&self, hash: &[u8; 32]) -> Option<&[u8]> {
        self.content.get(hash).map(Vec::as_slice)
    }

    pub fn terrain(&self) -> Option<&Terrain> {
        self.terrain.as_ref()
    }

    /// Чанки рельефа, изменившиеся с прошлого вызова.
    pub fn take_terrain_dirty(&mut self) -> Vec<astrocore_math::ChunkCoord> {
        std::mem::take(&mut self.terrain_dirty)
    }

    /// Растёт при каждом применении полного снимка рельефа: рендер обязан пересобрать всё.
    pub fn terrain_serial(&self) -> u32 {
        self.terrain_serial
    }

    pub fn snapshot_request_packet(&self) -> Vec<u8> {
        encode(&ClientMessage::RequestSnapshot).expect("валидно")
    }

    /// Один тик ввода: применяется локально (предсказание) и возвращается пакет для сервера.
    /// До готовности мира возвращает `None`.
    pub fn input_packet(&mut self, move_x: i8, move_z: i8, yaw: f32) -> Option<Vec<u8>> {
        self.you?;
        self.replica.as_ref()?;
        if self.pending.len() >= MAX_PENDING_INPUTS {
            return None; // сервер давно не отвечает — не копим бесконечно
        }
        self.seq += 1;
        let cmd = InputCmd {
            noclip: self.noclip,
            speed: self.speed,
            seq: self.seq,
            move_x,
            move_z,
            yaw,
        };
        let dt = self.dt();
        apply_input(
            &self.physics,
            self.terrain
                .as_ref()
                .map(|t| t as &dyn astrocore_physics::GroundProvider),
            &mut self.state,
            &self.shape,
            &cmd,
            dt,
        );
        self.pending.push_back(cmd);
        self.history.push_back((cmd.seq, self.state));
        encode(&ClientMessage::Input {
            seq: cmd.seq,
            move_x,
            move_z,
            yaw,
        })
        .ok()
    }

    fn dt(&self) -> f32 {
        1.0 / f32::from(self.tick_hz)
    }

    // ---- состояние ------------------------------------------------------------

    pub fn world(&self) -> Option<&World> {
        self.replica.as_ref()
    }

    pub fn net_id(&self) -> Option<NetworkEntityId> {
        self.you
    }

    /// Предсказанная позиция (ступни).
    /// Лететь сквозь преграды (призрак): предсказание движения должно совпадать с серверным.
    /// Множитель скорости ходьбы: предсказание должно совпадать с серверным.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    pub fn set_noclip(&mut self, on: bool) {
        self.noclip = on;
    }

    pub fn position(&self) -> Vec3 {
        self.state.pos
    }

    /// Регионы мира, изменившиеся с прошлого вызова (для пересборки мешей рендером).
    pub fn take_render_dirty(&mut self) -> Vec<astrocore_world::DirtyRegion> {
        std::mem::take(&mut self.render_dirty)
    }

    /// Растёт при каждом применении полного снимка: рендер обязан пересобрать всё.
    pub fn snapshot_serial(&self) -> u32 {
        self.snapshot_serial
    }

    pub fn is_ready(&self) -> bool {
        self.you.is_some() && self.replica.is_some()
    }

    pub fn pending_inputs(&self) -> usize {
        self.pending.len()
    }

    pub fn door_open(&self, net: NetworkEntityId) -> Option<bool> {
        self.doors.get(&net).map(|d| d.1)
    }

    /// Проём мира, соответствующий двери с данным сетевым идентификатором.
    pub fn door_element(&self, net: NetworkEntityId) -> Option<ObjectId> {
        self.doors.get(&net).map(|d| d.0)
    }

    /// Серверный тик последнего полученного состояния любой удалённой сущности.
    pub fn latest_remote_tick(&self) -> Option<u32> {
        self.remotes
            .values()
            .filter_map(|h| h.back().map(|s| s.0))
            .max()
    }

    /// Последний направленный угол удалённой сущности нужен рендеру для выбора кадра спрайта.
    /// Сетевые идентификаторы известных дверей.
    pub fn door_ids(&self) -> Vec<NetworkEntityId> {
        self.doors.keys().copied().collect()
    }

    pub fn remote_entities(&self) -> Vec<NetworkEntityId> {
        self.remotes.keys().copied().collect()
    }

    /// Интерполированное положение удалённой сущности на «времени рендера» (в тиках сервера).
    /// За пределами известной истории позиция фиксируется на крайних значениях.
    pub fn interpolated(&self, id: NetworkEntityId, render_tick: f32) -> Option<(Vec3, f32)> {
        let h = self.remotes.get(&id)?;
        let first = h.front()?;
        let last = h.back()?;
        if render_tick <= first.0 as f32 {
            return Some((first.1, first.2));
        }
        if render_tick >= last.0 as f32 {
            return Some((last.1, last.2));
        }
        let (a, b) = h
            .iter()
            .zip(h.iter().skip(1))
            .find(|(a, b)| a.0 as f32 <= render_tick && render_tick <= b.0 as f32)?;
        let t = (render_tick - a.0 as f32) / (b.0 - a.0).max(1) as f32;
        Some((a.1.lerp(b.1, t), a.2 + (b.2 - a.2) * t))
    }

    // ---- входящие пакеты ------------------------------------------------------

    pub fn on_packet(&mut self, packet: &[u8]) -> Result<Vec<ClientEvent>, ClientError> {
        let msg: ServerMessage = decode(packet)?;
        let mut events = Vec::new();
        match msg {
            ServerMessage::Welcome {
                tick_rate_hz, you, ..
            } => {
                self.tick_hz = tick_rate_hz.max(1);
                self.you = Some(you);
                self.reassembler.reset();
                events.push(ClientEvent::Joined { you });
            }
            ServerMessage::SnapshotPart { index, total, data } => {
                if let Some(state) = self.reassembler.push(index, total, data)? {
                    let world = World::from_state(state)?;
                    let revision = world.revision().0;
                    self.replica = Some(world);
                    self.snapshot_serial += 1;
                    self.render_dirty.clear();
                    self.rebuild_physics();
                    events.push(ClientEvent::SnapshotApplied { revision });
                    let buffered = std::mem::take(&mut self.buffered_ops);
                    for op in buffered {
                        self.apply_op(&op, &mut events);
                    }
                }
            }
            ServerMessage::WorldOp(op) => {
                if self.replica.is_none() {
                    if self.buffered_ops.len() >= MAX_BUFFERED_OPS {
                        self.buffered_ops.clear();
                        events.push(ClientEvent::ResyncRequested);
                    } else {
                        self.buffered_ops.push(op);
                    }
                } else {
                    self.apply_op(&op, &mut events);
                }
            }
            ServerMessage::TerrainSnapshotPart { index, total, data } => {
                if let Some(state) = self.terrain_reasm.push_decoded(index, total, data)? {
                    let mut t = Terrain::from_state(state)
                        .map_err(|_| ClientError::Unexpected("повреждённый снимок рельефа"))?;
                    t.take_dirty();
                    self.terrain = Some(t);
                    self.terrain_serial += 1;
                    self.terrain_dirty.clear();
                    events.push(ClientEvent::TerrainChanged);
                }
            }
            ServerMessage::TerrainOp { seq, data } => {
                let edit: Result<TerrainEdit, _> = postcard::from_bytes(&data);
                match (self.terrain.as_mut(), edit) {
                    (Some(t), Ok(edit)) => match t.apply_replicated(&TerrainOp { seq, edit }) {
                        Ok(true) => {
                            self.terrain_dirty.extend(t.take_dirty());
                            events.push(ClientEvent::TerrainChanged);
                        }
                        Ok(false) => {}
                        Err(_) => {
                            self.terrain = None; // сомнительное состояние — ждём новый снимок
                            self.terrain_reasm.reset();
                            events.push(ClientEvent::ResyncRequested);
                        }
                    },
                    (None, _) => events.push(ClientEvent::ResyncRequested),
                    (_, Err(_)) => {
                        return Err(ClientError::Unexpected("повреждённая правка рельефа"));
                    }
                }
            }
            ServerMessage::InputAck { seq, pos } => {
                self.reconcile(seq, Vec3::from_array(pos), &mut events)
            }
            ServerMessage::EntityState { id, pos, yaw, tick } => {
                let h = self.remotes.entry(id).or_default();
                if h.back().is_none_or(|l| tick >= l.0) {
                    h.push_back((tick, Vec3::from_array(pos), yaw));
                    while h.len() > MAX_REMOTE_SAMPLES {
                        h.pop_front();
                    }
                }
                events.push(ClientEvent::EntityUpdated(id));
            }
            ServerMessage::ComponentDelta {
                net,
                kind,
                seq,
                data,
                ..
            } => {
                // Последовательность: значение со старым номером (задержанный датаграммой пакет) игнорируется.
                let key = (net, kind.clone());
                if self.components.get(&key).is_none_or(|(old, _)| seq > *old) {
                    self.components.insert(key, (seq, data));
                    events.push(ClientEvent::ComponentChanged { net, kind });
                }
            }
            ServerMessage::ContentMissing { hash } => {
                self.content_partial.remove(&hash);
                events.push(ClientEvent::ContentFailed { hash });
            }
            ServerMessage::ContentPart {
                hash,
                index,
                total,
                data,
            } => {
                if self.content.contains_key(&hash) {
                    return Ok(events); // уже есть
                }
                if !self.content_partial.contains_key(&hash)
                    && self.content_partial.len() >= MAX_CONTENT_DOWNLOADS
                {
                    return Ok(events); // слишком много одновременных загрузок
                }
                let asm = self
                    .content_partial
                    .entry(hash)
                    .or_insert_with(|| ContentAssembly {
                        total,
                        parts: BTreeMap::new(),
                        bytes: 0,
                    });
                if asm.total != total {
                    self.content_partial.remove(&hash);
                    events.push(ClientEvent::ContentFailed { hash });
                    return Ok(events);
                }
                if asm.parts.insert(index, data.clone()).is_none() {
                    asm.bytes += data.len();
                }
                if asm.parts.len() as u32 == asm.total {
                    let asm = self.content_partial.remove(&hash).unwrap_or_default();
                    let blob: Vec<u8> = asm.parts.into_values().flatten().collect();
                    use sha2::{Digest, Sha256};
                    let actual: [u8; 32] = Sha256::digest(&blob).into();
                    if actual == hash {
                        self.content.insert(hash, blob);
                        events.push(ClientEvent::ContentReceived { hash });
                    } else {
                        events.push(ClientEvent::ContentFailed { hash });
                    }
                }
            }
            ServerMessage::EntityRemoved { id } => {
                self.components.retain(|(n, _), _| *n != id);
                self.remotes.remove(&id);
                events.push(ClientEvent::EntityRemoved(id));
            }
            ServerMessage::DoorState { net, element, open } => {
                self.doors.insert(net, (element, open));
                self.sync_physics_doors();
                events.push(ClientEvent::DoorChanged { net, open });
            }
            ServerMessage::RoundRestart => {
                self.restart();
                events.push(ClientEvent::RoundRestarted);
            }
            ServerMessage::Reject { reason, detail } => {
                events.push(ClientEvent::Rejected { reason, detail })
            }
        }
        Ok(events)
    }

    fn apply_op(&mut self, op: &WorldOp, events: &mut Vec<ClientEvent>) {
        let Some(world) = self.replica.as_mut() else {
            return;
        };
        match world.apply_replicated(op) {
            ReplicaOutcome::Applied => {
                let dirty = world.take_dirty();
                self.physics.rebuild_regions(world, &dirty);
                self.render_dirty.extend(dirty);
                self.sync_physics_doors();
                events.push(ClientEvent::WorldChanged { seq: op.seq });
            }
            ReplicaOutcome::Duplicate => {}
            ReplicaOutcome::Gap { .. } | ReplicaOutcome::Rejected(_) => {
                self.replica = None; // состояние сомнительно — ждём новый снимок
                self.reassembler.reset();
                events.push(ClientEvent::ResyncRequested);
            }
        }
    }

    fn rebuild_physics(&mut self) {
        if let Some(world) = self.replica.as_mut() {
            world.take_dirty();
            self.physics = BoxWorld::from_world(world);
        }
        self.sync_physics_doors();
    }

    fn sync_physics_doors(&mut self) {
        let Some(world) = self.replica.as_ref() else {
            return;
        };
        let open: BTreeMap<ObjectId, bool> = self.doors.values().copied().collect();
        self.physics
            .sync_doors(world, |id| open.get(&id).copied().unwrap_or(false));
    }

    /// Согласование предсказания с авторитетной позицией сервера.
    fn reconcile(&mut self, seq: u32, server_pos: Vec3, events: &mut Vec<ClientEvent>) {
        if seq == 0 {
            // Начальная синхронизация позиции сразу после входа.
            self.state = CharacterState::at(server_pos);
            self.pending.clear();
            self.history.clear();
            return;
        }
        while self.pending.front().is_some_and(|c| c.seq <= seq) {
            self.pending.pop_front();
        }
        let predicted = loop {
            match self.history.front() {
                Some((s, _)) if *s < seq => {
                    self.history.pop_front();
                }
                Some((s, st)) if *s == seq => {
                    let st = *st;
                    self.history.pop_front();
                    break Some(st);
                }
                _ => break None,
            }
        };
        let distance = predicted.map_or(f32::INFINITY, |p| p.pos.distance(server_pos));
        if distance <= CORRECTION_EPSILON_M {
            return;
        }
        // Расхождение: берём серверную позицию и проигрываем неподтверждённый ввод заново.
        let mut base = predicted.unwrap_or(self.state);
        base.pos = server_pos;
        let dt = self.dt();
        self.history.clear();
        for cmd in self.pending.iter().copied().collect::<Vec<_>>() {
            apply_input(
                &self.physics,
                self.terrain
                    .as_ref()
                    .map(|t| t as &dyn astrocore_physics::GroundProvider),
                &mut base,
                &self.shape,
                &cmd,
                dt,
            );
            self.history.push_back((cmd.seq, base));
        }
        self.state = base;
        events.push(ClientEvent::Corrected {
            distance: if distance.is_finite() { distance } else { 0.0 },
        });
    }
}
