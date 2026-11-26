//! Сетевые идентификаторы сущностей (`NetEntity` Robust): у сервера и клиента разные
//! локальные идентификаторы, по сети ходит общий номер.

use std::collections::HashMap;

use astrocore_ecs::GameTick;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

/// Сетевой номер сущности. Ноль — «нет сущности».
#[derive(
    Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct NetEntity(pub u32);

/// Сетевые номера мира. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct NetEntities {
    by_net: HashMap<NetEntity, Entity>,
    last: u32,
}

impl NetEntities {
    /// Локальная сущность по сетевому номеру.
    pub fn entity(&self, net: NetEntity) -> Option<Entity> {
        self.by_net.get(&net).copied()
    }

    /// Выдаёт следующий номер сущности сервера.
    pub(crate) fn allocate(&mut self, entity: Entity) -> NetEntity {
        self.last += 1;
        let net = NetEntity(self.last);
        self.by_net.insert(net, entity);
        net
    }

    /// Запоминает номер, пришедший с сервера.
    pub(crate) fn insert(&mut self, net: NetEntity, entity: Entity) {
        self.by_net.insert(net, entity);
    }

    pub(crate) fn remove(&mut self, net: NetEntity) {
        self.by_net.remove(&net);
    }

    pub fn len(&self) -> usize {
        self.by_net.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_net.is_empty()
    }
}

/// Отметки изменений сущности для сети (сервер).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NetSync {
    /// Тик последнего изменения любого сетевого компонента.
    pub last_modified: GameTick,
    /// Тик последнего удаления сетевого компонента.
    pub last_removed: GameTick,
}

/// Удалённые сущности сервера: клиенты, знавшие сущность, должны удалить её у себя.
#[derive(Resource, Debug, Default)]
pub struct DeletionLog {
    /// По возрастанию тиков.
    entries: Vec<(GameTick, Entity, NetEntity)>,
}

impl DeletionLog {
    pub(crate) fn push(&mut self, tick: GameTick, entity: Entity, net: NetEntity) {
        self.entries.push((tick, entity, net));
    }

    /// Удаления после тика `since`.
    pub(crate) fn since(&self, since: GameTick) -> &[(GameTick, Entity, NetEntity)] {
        let start = self.entries.partition_point(|(tick, ..)| *tick <= since);
        &self.entries[start..]
    }

    /// Забывает удаления не позже `tick`: их подтвердили все клиенты.
    pub(crate) fn prune(&mut self, tick: GameTick) {
        let keep_from = self.entries.partition_point(|(t, ..)| *t <= tick);
        self.entries.drain(..keep_from);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
