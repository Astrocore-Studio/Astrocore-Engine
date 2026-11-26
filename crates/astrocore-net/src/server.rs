//! Сервер: сессии игроков и отправка состояний.
//!
//! Образец — `ServerGameStateManager` и `PvsSystem` Robust @ 1afd692. Клиент подтверждает
//! применённые состояния. Каждое новое состояние строится от подтверждённого: для сущности,
//! которую клиент знает, уходят компоненты, изменившиеся после тика, на который она у него
//! подтверждена; для незнакомой — все. Поэтому потеря состояния ничего не ломает: следующее
//! несёт те же изменения.

use std::collections::{HashMap, VecDeque};

use astrocore_console::{CVarDef, CVarFlags, ConfigWorldExt};
use astrocore_ecs::{ChangeHistory, EventWorldExt, GameTick, GameTime, MetaData};
use astrocore_map::{Transform, TransformWorldExt};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::{With, Without};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use rayon::prelude::*;
use smallvec::SmallVec;

use crate::ids::{DeletionLog, NetEntities, NetEntity, NetSync};
use crate::protocol::{
    ClientMessage, EntityState, GameState, MAX_UNRELIABLE_BYTES, STATE_PART_BYTES, ServerMessage,
};
use crate::pvs::{PvsIndex, PvsOverrides, View, Visible, collect_visible};
use crate::registry::{NetRegistry, WriteContext};
use crate::visibility::VisibilityWorldExt;

/// Полуширина квадрата видимости в тайлах (`net.pvs_range` Robust).
pub const PVS_RANGE: CVarDef<f32> = CVarDef::new(
    "net.pvs_range",
    25.0,
    CVarFlags::SERVERONLY.union(CVarFlags::ARCHIVE),
    "Полуширина квадрата видимости игрока в тайлах.",
);

/// Сколько новых сущностей клиент получает за тик (`net.pvs_entity_budget` Robust).
pub const PVS_ENTITY_BUDGET: CVarDef<i32> = CVarDef::new(
    "net.pvs_entity_budget",
    200,
    CVarFlags::SERVERONLY.union(CVarFlags::ARCHIVE),
    "Сколько новых сущностей клиент получает за один тик.",
);

/// Номер соединения транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// Событие транспорта для сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Disconnected(ConnId),
    Packet(ConnId, Vec<u8>),
}

/// Пакет сервера клиенту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub conn: ConnId,
    /// Надёжный упорядоченный канал; иначе — последовательный ненадёжный.
    pub reliable: bool,
    pub bytes: Vec<u8>,
}

/// Почтовый ящик сервера: транспорт кладёт входящие и забирает исходящие. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct ServerNet {
    pub incoming: Vec<Incoming>,
    pub outgoing: Vec<Outgoing>,
    /// Адреса соединений: их сообщает транспорт.
    pub addresses: HashMap<ConnId, std::net::SocketAddr>,
    by_conn: HashMap<ConnId, Entity>,
    next_part: u32,
}

impl ServerNet {
    /// Сессия соединения.
    pub fn session(&self, conn: ConnId) -> Option<Entity> {
        self.by_conn.get(&conn).copied()
    }

    /// Сессии сервера.
    pub fn sessions(&self) -> impl Iterator<Item = Entity> + '_ {
        self.by_conn.values().copied()
    }

    /// Адрес соединения, если транспорт его сообщил.
    pub fn address(&self, conn: ConnId) -> Option<std::net::SocketAddr> {
        self.addresses.get(&conn).copied()
    }

    pub(crate) fn send(&mut self, conn: ConnId, reliable: bool, message: &ServerMessage) {
        if let Ok(bytes) = postcard::to_allocvec(message) {
            self.outgoing.push(Outgoing {
                conn,
                reliable,
                bytes,
            });
        }
    }
}

/// Что клиент знает о сущности. Ячейки лежат в массиве по индексу сущности: проверка
/// «знает ли клиент сущность» обходится без хеширования.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// Сущность, которой принадлежит ячейка: индекс мог перейти к другой сущности.
    entity: Entity,
    /// Тик последнего подтверждённого состояния, в котором сущность была видна клиенту.
    base: GameTick,
}

const EMPTY_SLOT: Slot = Slot {
    entity: Entity::PLACEHOLDER,
    base: GameTick::ZERO,
};

/// Учёт того, что знает клиент сессии.
#[derive(Debug, Default)]
pub struct SessionSync {
    /// Тик последнего подтверждённого состояния.
    acked: GameTick,
    /// Знания клиента о сущностях по индексу сущности.
    slots: Vec<Slot>,
    known_count: usize,
    /// Сущности в зоне видимости по последнему подтверждённому состоянию.
    in_view: Vec<Entity>,
    /// Отправленные и ещё не подтверждённые состояния: тик и видимые сущности.
    sent: VecDeque<(GameTick, Vec<Entity>)>,
    /// Освободившиеся списки: переиспользуются вместо новых выделений памяти.
    spare: Vec<Vec<Entity>>,
    /// Отметки «сущность видна в строящемся состоянии» по индексу сущности.
    seen: Vec<u32>,
    seen_stamp: u32,
    /// Что клиент знает о своей сущности: `None` — ещё не сообщали.
    attached_sent: Option<Option<NetEntity>>,
    scratch: Visible,
}

/// Сколько неподтверждённых состояний помнит сервер.
const MAX_UNACKED: usize = 64;

fn slot_index(entity: Entity) -> usize {
    entity.index_u32() as usize
}

impl SessionSync {
    /// Тик, на который сущность подтверждена у клиента; `None` — клиент её не знает.
    fn base(&self, entity: Entity) -> Option<GameTick> {
        self.slots
            .get(slot_index(entity))
            .filter(|slot| slot.entity == entity)
            .map(|slot| slot.base)
    }

    fn forget(&mut self, entity: Entity) {
        if let Some(slot) = self.slots.get_mut(slot_index(entity))
            && slot.entity == entity
        {
            *slot = EMPTY_SLOT;
            self.known_count -= 1;
        }
    }

    fn recycle(&mut self, mut list: Vec<Entity>) {
        list.clear();
        self.spare.push(list);
    }

    fn on_ack(&mut self, tick: GameTick, deletions: &DeletionLog) {
        if tick <= self.acked {
            return;
        }
        let Some(position) = self.sent.iter().position(|(sent, _)| *sent == tick) else {
            return;
        };
        // Удаления до подтверждённого тика клиент уже получил вместе с этим состоянием.
        for (_, entity, _) in deletions
            .since(self.acked)
            .iter()
            .take_while(|(deleted, ..)| *deleted <= tick)
        {
            self.forget(*entity);
        }
        for _ in 0..position {
            if let Some((_, stale)) = self.sent.pop_front() {
                self.recycle(stale);
            }
        }
        let (_, visible) = self.sent.pop_front().expect("позиция найдена");
        for &entity in &visible {
            let index = slot_index(entity);
            if index >= self.slots.len() {
                self.slots.resize(index + 1, EMPTY_SLOT);
            }
            let slot = &mut self.slots[index];
            if slot.entity != entity {
                if slot.entity == Entity::PLACEHOLDER {
                    self.known_count += 1;
                }
                slot.entity = entity;
            }
            slot.base = tick;
        }
        let previous = std::mem::replace(&mut self.in_view, visible);
        self.recycle(previous);
        self.acked = tick;
    }

    /// Клиент потерял состояние мира: всё отправляется заново.
    fn reset(&mut self) {
        self.acked = GameTick::ZERO;
        self.slots.clear();
        self.known_count = 0;
        self.in_view.clear();
        self.sent.clear();
        self.attached_sent = None;
    }

    /// Тик последнего подтверждённого состояния.
    pub fn acked(&self) -> GameTick {
        self.acked
    }

    /// Сколько сущностей знает клиент.
    pub fn known_entities(&self) -> usize {
        self.known_count
    }
}

/// Сессия игрока (`ICommonSession` Robust). Компонент сущности-сессии.
#[derive(Component, Debug)]
pub struct Session {
    pub conn: ConnId,
    pub name: String,
    /// Сущность, которой управляет игрок.
    pub attached: Option<Entity>,
    /// Номер последнего обработанного кадра ввода клиента: уходит ему с каждым состоянием.
    pub last_input: u32,
    pub sync: SessionSync,
}

/// Маркер сущности, которой управляет игрок (`ActorComponent` Robust).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub session: Entity,
}

/// Игрок вошёл. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct PlayerConnected {
    pub session: Entity,
}

/// Игрок вышел: сессия будет удалена после обработчиков. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct PlayerDisconnected {
    pub session: Entity,
}

/// Игрок получил сущность под управление. Поднимается на сущности и широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct PlayerAttached {
    pub session: Entity,
    pub entity: Entity,
}

/// Игрок перестал управлять сущностью. Поднимается на сущности и широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct PlayerDetached {
    pub session: Entity,
    pub entity: Entity,
}

/// Сессии на `World` сервера.
pub trait SessionWorldExt {
    /// Отдаёт сущность под управление сессии; прежний хозяин сущности и прежняя сущность
    /// сессии отсоединяются.
    fn attach_player(&mut self, session: Entity, entity: Entity);

    /// Забирает у сессии её сущность.
    fn detach_player(&mut self, session: Entity);

    /// Сессия, управляющая сущностью.
    fn session_of(&self, entity: Entity) -> Option<Entity>;
}

impl SessionWorldExt for World {
    fn attach_player(&mut self, session: Entity, entity: Entity) {
        if self.get::<Session>(session).is_none() || self.get_entity(entity).is_err() {
            return;
        }
        if let Some(previous) = self.get::<Actor>(entity).map(|actor| actor.session) {
            if previous == session {
                return;
            }
            self.detach_player(previous);
        }
        self.detach_player(session);
        if let Some(mut state) = self.get_mut::<Session>(session) {
            state.attached = Some(entity);
        }
        self.entity_mut(entity).insert(Actor { session });
        self.raise_and_broadcast(entity, &mut PlayerAttached { session, entity });
    }

    fn detach_player(&mut self, session: Entity) {
        let Some(mut state) = self.get_mut::<Session>(session) else {
            return;
        };
        let Some(entity) = state.attached.take() else {
            return;
        };
        if let Ok(mut target) = self.get_entity_mut(entity) {
            target.remove::<Actor>();
        }
        self.raise_and_broadcast(entity, &mut PlayerDetached { session, entity });
    }

    fn session_of(&self, entity: Entity) -> Option<Entity> {
        self.get::<Actor>(entity).map(|actor| actor.session)
    }
}

/// Разбирает входящие пакеты. Этап `PreUpdate`.
pub fn receive(world: &mut World) {
    let incoming = std::mem::take(&mut world.resource_mut::<ServerNet>().incoming);
    for event in incoming {
        match event {
            Incoming::Disconnected(conn) => disconnect(world, conn),
            Incoming::Packet(conn, bytes) => {
                let Ok(message) = postcard::from_bytes::<ClientMessage>(&bytes) else {
                    // Клиент, который шлёт мусор, отключается.
                    disconnect(world, conn);
                    continue;
                };
                handle(world, conn, message);
            }
        }
    }
}

fn disconnect(world: &mut World, conn: ConnId) {
    world.resource_mut::<ServerNet>().addresses.remove(&conn);
    let Some(session) = world.resource_mut::<ServerNet>().by_conn.remove(&conn) else {
        return;
    };
    world.raise_broadcast(&mut PlayerDisconnected { session });
    world.detach_player(session);
    world
        .resource_mut::<PvsOverrides>()
        .per_session
        .remove(&session);
    world.despawn(session);
}

fn handle(world: &mut World, conn: ConnId, message: ClientMessage) {
    let session = world.resource::<ServerNet>().session(conn);
    match (message, session) {
        (ClientMessage::Hello { name, fingerprint }, None) => {
            let expected = world.resource::<NetRegistry>().fingerprint();
            if fingerprint != expected {
                world.resource_mut::<ServerNet>().send(
                    conn,
                    true,
                    &ServerMessage::Rejected("у клиента другой набор сетевых компонентов".into()),
                );
                return;
            }
            let session = world
                .spawn(Session {
                    conn,
                    name,
                    attached: None,
                    last_input: 0,
                    sync: SessionSync::default(),
                })
                .id();
            let time = world.resource::<GameTime>();
            let welcome = ServerMessage::Welcome {
                tick_rate: time.tick_rate,
                tick: time.tick,
            };
            let mut net = world.resource_mut::<ServerNet>();
            net.by_conn.insert(conn, session);
            net.send(conn, true, &welcome);
            world.raise_broadcast(&mut PlayerConnected { session });
        }
        (ClientMessage::Ack(tick), Some(session)) => {
            world.resource_scope(|world, deletions: bevy_ecs::world::Mut<DeletionLog>| {
                if let Some(mut state) = world.get_mut::<Session>(session) {
                    state.sync.on_ack(tick, &deletions);
                }
            });
        }
        (ClientMessage::RequestFull, Some(session)) => {
            if let Some(mut state) = world.get_mut::<Session>(session) {
                state.sync.reset();
            }
        }
        (ClientMessage::Event { id, bytes }, Some(session)) => {
            crate::events::dispatch(world, Some(session), id, &bytes);
        }
        // Сообщения до приветствия и повторное приветствие игнорируются.
        _ => {}
    }
}

/// Выдаёт сетевые номера новым сущностям. Этап `Sync`.
pub(crate) fn assign_net_entities(world: &mut World) {
    let fresh: Vec<Entity> = world
        .query_filtered::<Entity, (With<MetaData>, Without<NetEntity>)>()
        .iter(world)
        .collect();
    if fresh.is_empty() {
        return;
    }
    let tick = world.resource::<GameTime>().tick;
    for entity in fresh {
        let net = world.resource_mut::<NetEntities>().allocate(entity);
        world.entity_mut(entity).insert((
            net,
            NetSync {
                last_modified: tick,
                last_removed: GameTick::ZERO,
            },
        ));
    }
}

/// Откуда смотрит сессия: положение её сущности.
fn view_of(world: &World, attached: Option<Entity>, range: f32) -> Option<View> {
    let entity = attached?;
    let map = world.get::<Transform>(entity)?.map()?;
    let placement = world.world_transform(entity)?;
    Some(View {
        map,
        position: placement.position,
        level: placement.level,
        range,
        mask: world.eye_mask(entity),
        viewer: Some(entity),
    })
}

struct Built {
    /// Закодированное сообщение с состоянием.
    bytes: Vec<u8>,
    visible: Vec<Entity>,
    attached: Option<Option<NetEntity>>,
}

/// Строит состояние для одной сессии. Мир только читается: сессии строятся параллельно.
fn build(
    world: &World,
    session_entity: Entity,
    session: &mut Session,
    tick: GameTick,
    range: f32,
    budget: usize,
) -> Built {
    let registry = world.resource::<NetRegistry>();
    let history = world.resource::<ChangeHistory>();
    let index = world.resource::<PvsIndex>();
    let overrides = world.resource::<PvsOverrides>();
    let deletions = world.resource::<DeletionLog>();
    let now = world.read_change_tick();
    let transform_id = world.component_id::<Transform>();
    let sync = &mut session.sync;

    let mut visible = std::mem::take(&mut sync.scratch);
    match view_of(world, session.attached, range) {
        Some(view) => collect_visible(world, index, &view, &mut visible),
        None => {
            visible.entities.clear();
            visible.groups.clear();
        }
    }
    // Отметки видимости: новая метка делает все прежние отметки недействительными.
    sync.seen_stamp = sync.seen_stamp.wrapping_add(1);
    if sync.seen_stamp == 0 {
        sync.seen.fill(0);
        sync.seen_stamp = 1;
    }
    let stamp = sync.seen_stamp;
    let mark = |seen: &mut Vec<u32>, entity: Entity| -> bool {
        let index = slot_index(entity);
        if index >= seen.len() {
            seen.resize(index + 1, 0);
        }
        std::mem::replace(&mut seen[index], stamp) != stamp
    };
    for &entity in &visible.entities {
        mark(&mut sync.seen, entity);
    }
    // Принудительно видимые сущности и сущность игрока — отдельными группами с предками.
    let forced = overrides
        .global
        .iter()
        .chain(
            overrides
                .per_session
                .get(&session_entity)
                .into_iter()
                .flatten(),
        )
        .chain(session.attached.iter());
    for &entity in forced {
        let mut chain: SmallVec<[Entity; 8]> = SmallVec::new();
        chain.push(entity);
        while let Some(parent) = world.parent(chain[chain.len() - 1]) {
            chain.push(parent);
        }
        for &ancestor in chain.iter().rev() {
            if mark(&mut sync.seen, ancestor) && world.get::<Transform>(ancestor).is_some() {
                visible.groups.push(visible.entities.len() as u32);
                visible.entities.push(ancestor);
            }
        }
    }

    let mut state = GameState {
        from: sync.acked,
        to: tick,
        last_input: session.last_input,
        deleted: deletions
            .since(sync.acked)
            .iter()
            .filter(|(_, entity, _)| sync.base(*entity).is_some())
            .map(|(_, _, net)| *net)
            .collect(),
        left: Vec::new(),
        entities: Vec::new(),
    };
    let mut sent_entities = sync.spare.pop().unwrap_or_default();
    sent_entities.reserve(visible.entities.len());
    let mut budget = budget;
    let mut scratch = Vec::new();
    for group in visible.iter_groups() {
        // Новая для клиента группа уходит целиком или не уходит вовсе.
        if sync.base(group[0]).is_none() {
            if budget == 0 {
                for &entity in group {
                    sync.seen[slot_index(entity)] = 0;
                }
                continue;
            }
            budget = budget.saturating_sub(group.len());
        }
        for &entity in group {
            let Ok(entity_ref) = world.get_entity(entity) else {
                continue;
            };
            let (Some(&net), Some(net_sync)) =
                (entity_ref.get::<NetEntity>(), entity_ref.get::<NetSync>())
            else {
                // Сущность создана в этом тике после выдачи номеров: уйдёт в следующем.
                sync.seen[slot_index(entity)] = 0;
                continue;
            };
            sent_entities.push(entity);
            let base = sync.base(entity);
            // Сущность вернулась в зону видимости: клиент убрал её из мира, поэтому
            // положение уходит заново, даже если оно не менялось.
            let reentering = base.is_some_and(|base| base != sync.acked);
            if !reentering && base.is_some_and(|base| net_sync.last_modified <= base) {
                continue;
            }
            let ctx = WriteContext {
                since: base.unwrap_or(GameTick::ZERO),
                session: session_entity,
            };
            let mut components = Vec::new();
            let mut present = Vec::new();
            for (net_id, info) in registry.iter() {
                let Some(ticks) = entity_ref.get_change_ticks_by_id(info.component_id) else {
                    continue;
                };
                present.push(net_id);
                let forced = reentering && Some(info.component_id) == transform_id;
                if !forced && base.is_some_and(|base| !history.changed_since(ticks, base, now)) {
                    continue;
                }
                scratch.clear();
                if info.write(world, entity, &ctx, &mut scratch) {
                    components.push((net_id, scratch.clone()));
                }
            }
            let removed = base.is_some_and(|base| net_sync.last_removed > base);
            if components.is_empty() && base.is_some() && !removed {
                continue;
            }
            state.entities.push(EntityState {
                net,
                prototype: base.is_none().then(|| {
                    entity_ref
                        .get::<MetaData>()
                        .and_then(|meta| meta.prototype.as_ref().map(|p| p.to_string()))
                }),
                present: (base.is_none() || removed).then_some(present),
                components,
            });
        }
    }
    state.left = sync
        .in_view
        .iter()
        .filter(|entity| sync.seen.get(slot_index(**entity)) != Some(&stamp))
        .filter_map(|entity| world.get::<NetEntity>(*entity).copied())
        .collect();
    sync.scratch = visible;

    let attached = session
        .attached
        .and_then(|e| world.get::<NetEntity>(e).copied());
    let attached = (sync.attached_sent != Some(attached)).then_some(attached);
    Built {
        bytes: crate::protocol::encode_state(state),
        visible: sent_entities,
        attached,
    }
}

/// Строит и отправляет состояния всем сессиям. Этап `Sync`.
pub(crate) fn send(world: &mut World) {
    let tick = world.resource::<GameTime>().tick;
    let range = world.cvar(&PVS_RANGE);
    let budget = usize::try_from(world.cvar(&PVS_ENTITY_BUDGET)).unwrap_or(0);
    let mut sessions: Vec<(Entity, Session)> = world
        .query::<(Entity, &Session)>()
        .iter(world)
        .map(|(entity, _)| entity)
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|entity| Some((entity, world.entity_mut(entity).take::<Session>()?)))
        .collect();
    if sessions.is_empty() {
        // Без клиентов журнал удалений никому не нужен.
        world.resource_mut::<DeletionLog>().prune(tick);
        return;
    }

    let shared: &World = world;
    let built: Vec<Built> = sessions
        .par_iter_mut()
        .map(|(entity, session)| build(shared, *entity, session, tick, range, budget))
        .collect();

    let mut oldest_ack = tick;
    for ((entity, mut session), built) in sessions.into_iter().zip(built) {
        let conn = session.conn;
        oldest_ack = oldest_ack.min(session.sync.acked);
        let mut net = world.resource_mut::<ServerNet>();
        if let Some(attached) = built.attached {
            net.send(conn, true, &ServerMessage::Attached(attached));
            session.sync.attached_sent = Some(attached);
        }
        let bytes = built.bytes;
        if !bytes.is_empty() {
            if bytes.len() <= MAX_UNRELIABLE_BYTES {
                net.outgoing.push(Outgoing {
                    conn,
                    reliable: false,
                    bytes,
                });
            } else if bytes.len() <= STATE_PART_BYTES {
                net.outgoing.push(Outgoing {
                    conn,
                    reliable: true,
                    bytes,
                });
            } else {
                // Большое состояние (вход в игру) режется на части надёжного канала.
                net.next_part += 1;
                let id = net.next_part;
                let count = bytes.len().div_ceil(STATE_PART_BYTES) as u32;
                for (index, chunk) in bytes.chunks(STATE_PART_BYTES).enumerate() {
                    net.send(
                        conn,
                        true,
                        &ServerMessage::StatePart {
                            id,
                            index: index as u32,
                            count,
                            bytes: chunk.to_vec(),
                        },
                    );
                }
            }
        }
        let sync = &mut session.sync;
        if sync.sent.len() == MAX_UNACKED
            && let Some((_, stale)) = sync.sent.pop_front()
        {
            sync.recycle(stale);
        }
        sync.sent.push_back((tick, built.visible));
        world.entity_mut(entity).insert(session);
    }
    world.resource_mut::<DeletionLog>().prune(oldest_ack);
}

/// Подписки сервера.
pub(crate) fn register(world: &mut World) {
    use astrocore_ecs::EntityTerminating;

    world.init_resource::<ServerNet>();
    world.init_resource::<NetEntities>();
    world.init_resource::<DeletionLog>();
    world.register_cvar(&PVS_RANGE);
    world.register_cvar(&PVS_ENTITY_BUDGET);
    world.subscribe::<NetEntity, EntityTerminating>(|world, entity, _| {
        let Some(net) = world.get::<NetEntity>(entity).copied() else {
            return;
        };
        let tick = world.resource::<GameTime>().tick;
        world.resource_mut::<NetEntities>().remove(net);
        world.resource_mut::<DeletionLog>().push(tick, entity, net);
    });
    // Удалённая сущность игрока освобождает сессию.
    world.subscribe::<Actor, EntityTerminating>(|world, entity, _| {
        if let Some(session) = world.session_of(entity) {
            world.detach_player(session);
        }
    });
}
