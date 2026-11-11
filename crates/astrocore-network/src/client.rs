//! Клиентское ядро: реплика мира, предсказание движения, интерполяция.
//!
//! Не знает про сокеты: принимает пакеты ([`ClientCore::on_packet`]) и отдаёт
//! готовые пакеты для отправки. Движение рассчитывается той же функцией, что и на
//! сервере ([`crate::movement::apply_input`]), поэтому в одинаковом мире
//! предсказание совпадает с серверным результатом; при расхождении клиент
//! принимает серверную позицию и заново проигрывает неподтверждённый ввод.

use std::collections::{BTreeMap, VecDeque};

use astrocore_core::{NetworkEntityId, ObjectId};
use astrocore_math::glam::Vec3;
use astrocore_physics::{BoxWorld, CharacterShape, CharacterState};
use astrocore_protocol::{
    ClientMessage, ProtocolError, RejectReason, ServerMessage, decode, encode,
};
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

pub struct ClientCore {
    build: String,
    world_id: String,
    known_world_hash: u64,
    shape: CharacterShape,
    tick_hz: u8,
    you: Option<NetworkEntityId>,
    replica: Option<World>,
    physics: BoxWorld,
    reassembler: Reassembler,
    buffered_ops: Vec<WorldOp>,
    doors: BTreeMap<NetworkEntityId, (ObjectId, bool)>,
    state: CharacterState,
    seq: u32,
    pending: VecDeque<InputCmd>,
    history: VecDeque<(u32, CharacterState)>,
    remotes: BTreeMap<NetworkEntityId, VecDeque<(u32, Vec3, f32)>>,
    render_dirty: Vec<astrocore_world::DirtyRegion>,
    snapshot_serial: u32,
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
            replica: None,
            physics: BoxWorld::default(),
            reassembler: Reassembler::new(),
            buffered_ops: Vec::new(),
            doors: BTreeMap::new(),
            state: CharacterState::at(Vec3::ZERO),
            seq: 0,
            pending: VecDeque::new(),
            history: VecDeque::new(),
            remotes: BTreeMap::new(),
            render_dirty: Vec::new(),
            snapshot_serial: 0,
        }
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
            seq: self.seq,
            move_x,
            move_z,
            yaw,
        };
        let dt = self.dt();
        apply_input(&self.physics, &mut self.state, &self.shape, &cmd, dt);
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
            ServerMessage::EntityRemoved { id } => {
                self.remotes.remove(&id);
                events.push(ClientEvent::EntityRemoved(id));
            }
            ServerMessage::DoorState { net, element, open } => {
                self.doors.insert(net, (element, open));
                self.sync_physics_doors();
                events.push(ClientEvent::DoorChanged { net, open });
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
            apply_input(&self.physics, &mut base, &self.shape, &cmd, dt);
            self.history.push_back((cmd.seq, base));
        }
        self.state = base;
        events.push(ClientEvent::Corrected {
            distance: if distance.is_finite() { distance } else { 0.0 },
        });
    }
}
