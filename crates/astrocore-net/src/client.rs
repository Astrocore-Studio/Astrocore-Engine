//! Клиент: применение состояний сервера. Образец — `ClientGameStateManager` Robust @ 1afd692.

use std::collections::HashMap;

use astrocore_ecs::{EventWorldExt, GameTick, LifecycleWorldExt, MetaData};
use astrocore_map::{Transform, TransformWorldExt};
use astrocore_prototypes::PrototypeWorldExt;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use crate::ids::{NetEntities, NetEntity};
use crate::protocol::{ClientMessage, GameState, MAX_STATE_PARTS, ServerMessage};
use crate::registry::{NetError, NetRegistry};

/// Состояние соединения клиента.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClientStatus {
    #[default]
    Offline,
    /// Приветствие отправлено, ответа ещё нет.
    Connecting,
    Connected,
    /// Сервер отказал: причина.
    Rejected(String),
}

/// Почтовый ящик клиента: транспорт кладёт входящие и забирает исходящие. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct ClientNet {
    pub incoming: Vec<Vec<u8>>,
    /// Пакеты серверу: признак надёжного канала и данные.
    pub outgoing: Vec<(bool, Vec<u8>)>,
    pub status: ClientStatus,
    /// Тик последнего применённого состояния.
    pub applied: GameTick,
    /// Частота тиков сервера.
    pub tick_rate: u16,
    /// Сущность, которой управляет игрок.
    pub attached: Option<Entity>,
    attached_net: Option<NetEntity>,
    /// Собираемые части большого состояния: номер → части.
    parts: HashMap<u32, Vec<Option<Vec<u8>>>>,
}

impl ClientNet {
    pub(crate) fn send(&mut self, reliable: bool, message: &ClientMessage) {
        if let Ok(bytes) = postcard::to_allocvec(message) {
            self.outgoing.push((reliable, bytes));
        }
    }
}

/// Маркер сущности, вышедшей из зоны видимости: она хранится у клиента, но нигде не находится.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Detached;

/// Клиент применил состояние сервера. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct StateApplied {
    pub tick: GameTick,
}

/// Сервер сменил сущность игрока. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct LocalPlayerAttached {
    pub entity: Option<Entity>,
}

/// Клиент на `World`.
pub trait ClientWorldExt {
    /// Отправляет серверу приветствие.
    fn net_connect(&mut self, name: &str);
}

impl ClientWorldExt for World {
    fn net_connect(&mut self, name: &str) {
        let fingerprint = self.resource::<NetRegistry>().fingerprint();
        let mut net = self.resource_mut::<ClientNet>();
        net.status = ClientStatus::Connecting;
        net.send(
            true,
            &ClientMessage::Hello {
                name: name.to_owned(),
                fingerprint,
            },
        );
    }
}

/// Разбирает входящие пакеты. Этап `PreUpdate`.
pub(crate) fn receive(world: &mut World) {
    let incoming = std::mem::take(&mut world.resource_mut::<ClientNet>().incoming);
    for bytes in incoming {
        let Ok(message) = postcard::from_bytes::<ServerMessage>(&bytes) else {
            continue;
        };
        handle(world, message);
    }
    resolve_attached(world);
}

fn handle(world: &mut World, message: ServerMessage) {
    match message {
        ServerMessage::Welcome { tick_rate, .. } => {
            let mut net = world.resource_mut::<ClientNet>();
            net.status = ClientStatus::Connected;
            net.tick_rate = tick_rate;
        }
        ServerMessage::Rejected(reason) => {
            world.resource_mut::<ClientNet>().status = ClientStatus::Rejected(reason);
        }
        ServerMessage::State(state) => apply_or_resync(world, state),
        ServerMessage::StatePart {
            id,
            index,
            count,
            bytes,
        } => {
            if count == 0 || count > MAX_STATE_PARTS || index >= count {
                return;
            }
            let mut net = world.resource_mut::<ClientNet>();
            let parts = net
                .parts
                .entry(id)
                .or_insert_with(|| vec![None; count as usize]);
            if parts.len() != count as usize {
                return;
            }
            parts[index as usize] = Some(bytes);
            if parts.iter().any(Option::is_none) {
                return;
            }
            let whole: Vec<u8> = net
                .parts
                .remove(&id)
                .into_iter()
                .flatten()
                .flatten()
                .flatten()
                .collect();
            if let Ok(ServerMessage::State(state)) = postcard::from_bytes(&whole) {
                apply_or_resync(world, state);
            }
        }
        ServerMessage::Attached(net_entity) => {
            world.resource_mut::<ClientNet>().attached_net = net_entity;
            if net_entity.is_none() {
                set_attached(world, None);
            }
        }
        ServerMessage::Event { id, bytes } => crate::events::dispatch(world, None, id, &bytes),
    }
}

fn set_attached(world: &mut World, entity: Option<Entity>) {
    let mut net = world.resource_mut::<ClientNet>();
    if net.attached == entity {
        return;
    }
    net.attached = entity;
    world.raise_broadcast(&mut LocalPlayerAttached { entity });
}

/// Сущность игрока могла прийти позже сообщения о ней.
fn resolve_attached(world: &mut World) {
    let Some(net_entity) = world.resource::<ClientNet>().attached_net else {
        return;
    };
    if let Some(entity) = world.resource::<NetEntities>().entity(net_entity) {
        set_attached(world, Some(entity));
    }
}

fn apply_or_resync(world: &mut World, state: GameState) {
    let applied = world.resource::<ClientNet>().applied;
    // Старое состояние или состояние от базы, которой у клиента нет, пропускается: сервер
    // пришлёт новое от подтверждённого тика.
    if state.to <= applied || state.from > applied {
        return;
    }
    let tick = state.to;
    match apply_state(world, state) {
        Ok(()) => {
            let mut net = world.resource_mut::<ClientNet>();
            net.applied = tick;
            net.send(false, &ClientMessage::Ack(tick));
            world.raise_broadcast(&mut StateApplied { tick });
        }
        Err(_) => {
            // Мир клиента разошёлся с сервером: просим всё заново.
            let mut net = world.resource_mut::<ClientNet>();
            net.applied = GameTick::ZERO;
            net.send(true, &ClientMessage::RequestFull);
        }
    }
}

fn spawn_from_state(world: &mut World, net: NetEntity, prototype: Option<&str>) -> Entity {
    let entity = prototype
        .and_then(|id| world.spawn_prototype(id).ok())
        .unwrap_or_else(|| world.spawn_uninitialized());
    if let Some(prototype) = prototype
        && let Some(mut meta) = world.get_mut::<MetaData>(entity)
        && meta.prototype.is_none()
    {
        // Прототипа нет в данных клиента: имя сохраняется для диагностики.
        meta.prototype = Some(prototype.into());
    }
    if world.get::<Transform>(entity).is_none() {
        world.entity_mut(entity).insert(Transform::default());
    }
    world.entity_mut(entity).insert(net);
    world.resource_mut::<NetEntities>().insert(net, entity);
    entity
}

/// Применяет состояние сервера к миру клиента.
pub fn apply_state(world: &mut World, state: GameState) -> Result<(), NetError> {
    for net in &state.deleted {
        if let Some(entity) = world.resource::<NetEntities>().entity(*net) {
            world.resource_mut::<NetEntities>().remove(*net);
            world.delete_entity(entity);
        }
    }

    let mut created = Vec::new();
    let mut deferred: Vec<(Entity, u16, &[u8])> = Vec::new();
    for entity_state in &state.entities {
        let known = world.resource::<NetEntities>().entity(entity_state.net);
        let entity = match (known, &entity_state.prototype) {
            (Some(entity), _) => entity,
            (None, Some(prototype)) => {
                let entity = spawn_from_state(world, entity_state.net, prototype.as_deref());
                created.push(entity);
                entity
            }
            // Сервер считает, что сущность у клиента есть, а её нет.
            (None, None) => return Err(NetError::UnknownEntity(entity_state.net.0)),
        };
        if world.get::<Detached>(entity).is_some() {
            world.entity_mut(entity).remove::<Detached>();
        }
        if let Some(present) = &entity_state.present {
            let stale: Vec<_> = world
                .resource::<NetRegistry>()
                .iter()
                .filter(|(net_id, info)| {
                    !present.contains(net_id)
                        && world
                            .get_entity(entity)
                            .is_ok_and(|e| e.contains_id(info.component_id))
                })
                .map(|(_, info)| info.component_id)
                .collect();
            for component in stale {
                world.remove_comp_id(entity, component);
            }
        }
        for (net_id, bytes) in &entity_state.components {
            let info = world
                .resource::<NetRegistry>()
                .get(*net_id)
                .cloned()
                .ok_or(NetError::UnknownComponent(*net_id))?;
            if info.deferred {
                deferred.push((entity, *net_id, bytes));
            } else {
                info.apply(world, entity, bytes)?;
            }
        }
    }
    for (entity, net_id, bytes) in deferred {
        if let Some(info) = world.resource::<NetRegistry>().get(net_id).cloned() {
            info.apply(world, entity, bytes)?;
        }
    }
    // Новые сущности инициализируются, когда все их компоненты и родители на месте.
    for entity in created {
        world.initialize_and_start(entity, false);
    }
    for net in &state.left {
        if let Some(entity) = world.resource::<NetEntities>().entity(*net) {
            world.detach_to_null(entity);
            world.entity_mut(entity).insert(Detached);
        }
    }
    Ok(())
}

/// Ресурсы клиента.
pub(crate) fn register(world: &mut World) {
    world.init_resource::<ClientNet>();
    world.init_resource::<NetEntities>();
}
