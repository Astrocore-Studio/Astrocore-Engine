//! Контейнеры: сущности внутри других сущностей (руки, карманы, шкафы, слоты машин).
//! Образец — `Robust.Shared/Containers` @ 1afd692.
//!
//! Содержимое контейнера — потомки владельца в иерархии с маркером [`InContainer`].
//! Вставлять и вынимать нужно через [`ContainerWorldExt`]: так поднимаются события попытки
//! и результата.

use std::collections::BTreeMap;
use std::sync::Arc;

use astrocore_ecs::{EventWorldExt, LifecycleWorldExt, MetaData};
use astrocore_math::Vec2;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::coords::EntityCoordinates;
use crate::map::MapWorldExt;
use crate::transform::{EntParentChanged, Transform, TransformWorldExt};

/// Вид контейнера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerKind {
    /// Любое число сущностей (`Container` Robust).
    #[default]
    List,
    /// Не больше одной сущности (`ContainerSlot` Robust).
    Slot,
}

/// Один контейнер сущности.
#[derive(Debug, Clone, Default)]
pub struct Container {
    pub kind: ContainerKind,
    pub(crate) entities: Vec<Entity>,
    /// Содержимое видно снаружи (рисуется поверх владельца).
    pub show_ents: bool,
    /// Содержимое не светит наружу.
    pub occludes_light: bool,
}

impl Container {
    pub fn new(kind: ContainerKind) -> Self {
        Self {
            kind,
            entities: Vec::new(),
            show_ents: false,
            occludes_light: true,
        }
    }

    /// Сущности в контейнере в порядке вставки.
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn contains(&self, entity: Entity) -> bool {
        self.entities.contains(&entity)
    }
}

/// Запись контейнера в данных: `!type:Container` или `!type:ContainerSlot`.
#[derive(Serialize, Deserialize)]
enum ContainerData {
    #[serde(rename = "Container")]
    List(ContainerFields),
    #[serde(rename = "ContainerSlot")]
    Slot(ContainerFields),
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerFields {
    #[serde(default)]
    show_ents: bool,
    #[serde(default = "default_true", rename = "occludes")]
    occludes_light: bool,
}

impl Serialize for Container {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = ContainerFields {
            show_ents: self.show_ents,
            occludes_light: self.occludes_light,
        };
        match self.kind {
            ContainerKind::List => ContainerData::List(fields),
            ContainerKind::Slot => ContainerData::Slot(fields),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Container {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (kind, fields) = match ContainerData::deserialize(deserializer)? {
            ContainerData::List(fields) => (ContainerKind::List, fields),
            ContainerData::Slot(fields) => (ContainerKind::Slot, fields),
        };
        Ok(Self {
            kind,
            entities: Vec::new(),
            show_ents: fields.show_ents,
            occludes_light: fields.occludes_light,
        })
    }
}

/// Контейнеры сущности по именам. В данных — компонент `ContainerContainer`.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerManager {
    pub containers: BTreeMap<String, Container>,
}

/// Имя компонента контейнеров в прототипах и картах SS14.
pub const CONTAINER_COMPONENT: &str = "ContainerContainer";

/// Маркер сущности внутри контейнера.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct InContainer {
    pub owner: Entity,
    pub id: Arc<str>,
}

/// Можно ли вставить сущность: поднимается на владельце контейнера.
#[derive(Debug, Clone)]
pub struct ContainerIsInsertingAttempt {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
    pub cancelled: bool,
}

/// Можно ли вставить сущность: поднимается на самой сущности.
#[derive(Debug, Clone)]
pub struct ContainerGettingInsertedAttempt {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
    pub cancelled: bool,
}

/// Можно ли вынуть сущность: поднимается на владельце контейнера.
#[derive(Debug, Clone)]
pub struct ContainerIsRemovingAttempt {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
    pub cancelled: bool,
}

/// Можно ли вынуть сущность: поднимается на самой сущности.
#[derive(Debug, Clone)]
pub struct ContainerGettingRemovedAttempt {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
    pub cancelled: bool,
}

/// Сущность вставлена: поднимается на владельце и широковещательно.
#[derive(Debug, Clone)]
pub struct EntInsertedIntoContainer {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
    pub old_parent: Option<Entity>,
}

/// Сущность вставлена: поднимается на самой сущности.
#[derive(Debug, Clone)]
pub struct EntGotInsertedIntoContainer {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
}

/// Сущность вынута: поднимается на владельце и широковещательно.
#[derive(Debug, Clone)]
pub struct EntRemovedFromContainer {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
}

/// Сущность вынута: поднимается на самой сущности.
#[derive(Debug, Clone)]
pub struct EntGotRemovedFromContainer {
    pub owner: Entity,
    pub container: Arc<str>,
    pub entity: Entity,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerError {
    #[error("у сущности нет контейнера с таким именем")]
    NoContainer,
    #[error("сущность не существует, удаляется или не имеет Transform")]
    BadEntity,
    #[error("слот уже занят")]
    SlotFull,
    #[error("сущность нельзя положить в саму себя или в своё содержимое")]
    Cycle,
    #[error("сущность — карта или грид")]
    NotInsertable,
    #[error("вставку отменил обработчик события")]
    Cancelled,
    #[error("сущность не в контейнере")]
    NotContained,
}

fn usable(world: &World, entity: Entity) -> bool {
    world.get::<Transform>(entity).is_some()
        && world
            .get::<MetaData>(entity)
            .is_none_or(|meta| !meta.terminating())
}

/// Убирает сущность из списка её контейнера и снимает маркер, без событий и перемещения.
fn forget(world: &mut World, entity: Entity) -> Option<InContainer> {
    let marker = world.get::<InContainer>(entity)?.clone();
    if let Some(mut manager) = world.get_mut::<ContainerManager>(marker.owner)
        && let Some(container) = manager.containers.get_mut(&*marker.id)
    {
        container.entities.retain(|e| *e != entity);
    }
    world.entity_mut(entity).remove::<InContainer>();
    Some(marker)
}

fn raise_removed(world: &mut World, entity: Entity, marker: InContainer) {
    world.raise_and_broadcast(
        marker.owner,
        &mut EntRemovedFromContainer {
            owner: marker.owner,
            container: marker.id.clone(),
            entity,
        },
    );
    world.raise(
        entity,
        &mut EntGotRemovedFromContainer {
            owner: marker.owner,
            container: marker.id,
            entity,
        },
    );
}

/// Контейнеры на `World`.
pub trait ContainerWorldExt {
    /// Создаёт контейнер, если его нет. `true` — создан.
    fn ensure_container(&mut self, owner: Entity, id: &str, kind: ContainerKind) -> bool;

    /// Контейнер сущности по имени.
    fn container(&self, owner: Entity, id: &str) -> Option<&Container>;

    /// Владелец и имя контейнера, в котором лежит сущность.
    fn containing_container(&self, entity: Entity) -> Option<&InContainer>;

    /// Сущность лежит в контейнере — прямо или внутри другой сущности в контейнере.
    fn is_in_container_or_nested(&self, entity: Entity) -> bool;

    /// Проверяет, можно ли вставить сущность, включая события попытки.
    fn can_insert(&mut self, entity: Entity, owner: Entity, id: &str)
    -> Result<(), ContainerError>;

    /// Вставляет сущность в контейнер; из прежнего контейнера она вынимается.
    fn insert_into_container(
        &mut self,
        entity: Entity,
        owner: Entity,
        id: &str,
    ) -> Result<(), ContainerError>;

    /// Вставляет без событий попытки (загрузка карты, стартовое содержимое).
    fn force_insert_into_container(
        &mut self,
        entity: Entity,
        owner: Entity,
        id: &str,
    ) -> Result<(), ContainerError>;

    /// Вынимает сущность из её контейнера. `destination: None` — рядом с владельцем:
    /// на его грид или карту.
    fn remove_from_container(
        &mut self,
        entity: Entity,
        destination: Option<EntityCoordinates>,
    ) -> Result<(), ContainerError>;

    /// Вынимает всё содержимое контейнера рядом с владельцем; возвращает вынутые сущности.
    fn empty_container(&mut self, owner: Entity, id: &str) -> Vec<Entity>;
}

fn insert(
    world: &mut World,
    entity: Entity,
    owner: Entity,
    id: &str,
    check_events: bool,
) -> Result<(), ContainerError> {
    structural_check(world, entity, owner, id)?;
    let container: Arc<str> = Arc::from(id);
    if check_events {
        attempt_insert(world, entity, owner, &container)?;
    }
    let old_parent = world.parent(entity);
    // Из прежнего контейнера сущность выходит с событиями, но без промежуточного места.
    if let Some(previous) = forget(world, entity) {
        raise_removed(world, entity, previous);
    }
    world
        .set_coordinates(entity, EntityCoordinates::new(owner, Vec2::ZERO), None)
        .map_err(|_| ContainerError::BadEntity)?;
    let Some(mut manager) = world.get_mut::<ContainerManager>(owner) else {
        return Err(ContainerError::NoContainer);
    };
    let Some(slot) = manager.containers.get_mut(id) else {
        return Err(ContainerError::NoContainer);
    };
    slot.entities.push(entity);
    world.entity_mut(entity).insert(InContainer {
        owner,
        id: container.clone(),
    });
    world.raise_and_broadcast(
        owner,
        &mut EntInsertedIntoContainer {
            owner,
            container: container.clone(),
            entity,
            old_parent,
        },
    );
    world.raise(
        entity,
        &mut EntGotInsertedIntoContainer {
            owner,
            container,
            entity,
        },
    );
    Ok(())
}

fn structural_check(
    world: &World,
    entity: Entity,
    owner: Entity,
    id: &str,
) -> Result<(), ContainerError> {
    if !usable(world, entity) || !usable(world, owner) {
        return Err(ContainerError::BadEntity);
    }
    let container = world
        .get::<ContainerManager>(owner)
        .and_then(|manager| manager.containers.get(id))
        .ok_or(ContainerError::NoContainer)?;
    if container.contains(entity) {
        return Ok(());
    }
    if container.kind == ContainerKind::Slot && !container.is_empty() {
        return Err(ContainerError::SlotFull);
    }
    if world.get::<crate::map::Map>(entity).is_some()
        || world.get::<crate::grid::MapGrid>(entity).is_some()
    {
        return Err(ContainerError::NotInsertable);
    }
    if entity == owner || world.is_ancestor(entity, owner) {
        return Err(ContainerError::Cycle);
    }
    Ok(())
}

fn attempt_insert(
    world: &mut World,
    entity: Entity,
    owner: Entity,
    container: &Arc<str>,
) -> Result<(), ContainerError> {
    let mut inserting = ContainerIsInsertingAttempt {
        owner,
        container: container.clone(),
        entity,
        cancelled: false,
    };
    world.raise(owner, &mut inserting);
    if inserting.cancelled {
        return Err(ContainerError::Cancelled);
    }
    let mut getting = ContainerGettingInsertedAttempt {
        owner,
        container: container.clone(),
        entity,
        cancelled: false,
    };
    world.raise(entity, &mut getting);
    if getting.cancelled {
        return Err(ContainerError::Cancelled);
    }
    Ok(())
}

impl ContainerWorldExt for World {
    fn ensure_container(&mut self, owner: Entity, id: &str, kind: ContainerKind) -> bool {
        self.ensure_comp::<ContainerManager>(owner);
        let Some(mut manager) = self.get_mut::<ContainerManager>(owner) else {
            return false;
        };
        if manager.containers.contains_key(id) {
            return false;
        }
        manager
            .containers
            .insert(id.to_owned(), Container::new(kind));
        true
    }

    fn container(&self, owner: Entity, id: &str) -> Option<&Container> {
        self.get::<ContainerManager>(owner)?.containers.get(id)
    }

    fn containing_container(&self, entity: Entity) -> Option<&InContainer> {
        self.get::<InContainer>(entity)
    }

    fn is_in_container_or_nested(&self, entity: Entity) -> bool {
        let mut current = Some(entity);
        for _ in 0..64 {
            let Some(found) = current else {
                return false;
            };
            if self.get::<InContainer>(found).is_some() {
                return true;
            }
            current = self.parent(found);
        }
        false
    }

    fn can_insert(
        &mut self,
        entity: Entity,
        owner: Entity,
        id: &str,
    ) -> Result<(), ContainerError> {
        structural_check(self, entity, owner, id)?;
        attempt_insert(self, entity, owner, &Arc::from(id))
    }

    fn insert_into_container(
        &mut self,
        entity: Entity,
        owner: Entity,
        id: &str,
    ) -> Result<(), ContainerError> {
        if self
            .get::<InContainer>(entity)
            .is_some_and(|marker| marker.owner == owner && &*marker.id == id)
        {
            return Ok(());
        }
        insert(self, entity, owner, id, true)
    }

    fn force_insert_into_container(
        &mut self,
        entity: Entity,
        owner: Entity,
        id: &str,
    ) -> Result<(), ContainerError> {
        if self
            .get::<InContainer>(entity)
            .is_some_and(|marker| marker.owner == owner && &*marker.id == id)
        {
            return Ok(());
        }
        insert(self, entity, owner, id, false)
    }

    fn remove_from_container(
        &mut self,
        entity: Entity,
        destination: Option<EntityCoordinates>,
    ) -> Result<(), ContainerError> {
        let marker = self
            .get::<InContainer>(entity)
            .cloned()
            .ok_or(ContainerError::NotContained)?;
        let mut removing = ContainerIsRemovingAttempt {
            owner: marker.owner,
            container: marker.id.clone(),
            entity,
            cancelled: false,
        };
        self.raise(marker.owner, &mut removing);
        let mut getting = ContainerGettingRemovedAttempt {
            owner: marker.owner,
            container: marker.id.clone(),
            entity,
            cancelled: false,
        };
        self.raise(entity, &mut getting);
        if removing.cancelled || getting.cancelled {
            return Err(ContainerError::Cancelled);
        }
        // Место по умолчанию — там, где стоит владелец: на его гриде или в космосе карты.
        let destination = destination.or_else(|| {
            let at = self.map_coordinates(marker.owner)?;
            self.snap_to_grid_or_map(at)
        });
        forget(self, entity);
        match destination {
            Some(destination) => {
                let _ = self.set_coordinates(entity, destination, None);
            }
            None => self.detach_to_null(entity),
        }
        raise_removed(self, entity, marker);
        Ok(())
    }

    fn empty_container(&mut self, owner: Entity, id: &str) -> Vec<Entity> {
        let entities: Vec<Entity> = self
            .container(owner, id)
            .map(|container| container.entities.clone())
            .unwrap_or_default();
        entities
            .into_iter()
            .filter(|entity| self.remove_from_container(*entity, None).is_ok())
            .collect()
    }
}

/// Подписки жизненного цикла контейнеров.
pub(crate) fn register(world: &mut World) {
    use astrocore_ecs::{EntityTerminating, RegistryWorldExt};

    world.register_data_component_as::<ContainerManager>(CONTAINER_COMPONENT);
    // Удаляемая сущность покидает контейнер: в списке владельца не остаётся мёртвых ссылок.
    world.subscribe::<InContainer, EntityTerminating>(|world, entity, _| {
        let owner_alive = world
            .get::<InContainer>(entity)
            .is_some_and(|marker| world.is_alive(marker.owner));
        if let Some(marker) = forget(world, entity)
            && owner_alive
        {
            raise_removed(world, entity, marker);
        }
    });
    // Сущность переместили в обход контейнера: список владельца приводится в порядок.
    world.subscribe::<InContainer, EntParentChanged>(|world, entity, _| {
        let moved_away = world
            .get::<InContainer>(entity)
            .is_some_and(|marker| world.parent(entity) != Some(marker.owner));
        if moved_away && let Some(marker) = forget(world, entity) {
            raise_removed(world, entity, marker);
        }
    });
}
