//! Положение сущности в иерархии. Образец — `TransformComponent` и `SharedTransformSystem`
//! Robust @ 1afd692.
//!
//! Родителя хранит связь `ChildOf` из `bevy_ecs`: карта → грид → сущность → содержимое
//! контейнера. [`Transform`] хранит положение относительно родителя и этаж. Менять положение
//! нужно через [`TransformWorldExt`]: так обновляются привязка к клетке, пауза и события.

use astrocore_ecs::{EventWorldExt, LifecycleWorldExt, MetaData};
use astrocore_math::{Angle, Vec2};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::coords::{EntityCoordinates, MapCoordinates, TileCoord};
use crate::grid::MapGrid;
use crate::map::Map;

/// Положение сущности относительно родителя.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Transform {
    #[serde(rename = "pos")]
    pub(crate) local_position: Vec2,
    #[serde(rename = "rot")]
    pub(crate) local_rotation: Angle,
    /// Этаж относительно родителя.
    pub(crate) z: i16,
    /// Сущность привязана к клетке грида.
    pub(crate) anchored: bool,
    /// Сущность не поворачивается: локальный поворот всегда ноль.
    pub(crate) no_rot: bool,
    /// Сущность карты, на которой находится эта сущность.
    #[serde(skip)]
    pub(crate) map: Option<Entity>,
    /// Грид, на котором находится эта сущность.
    #[serde(skip)]
    pub(crate) grid: Option<Entity>,
    /// Привязка записана в клетку грида.
    #[serde(skip)]
    pub(crate) anchor_registered: bool,
}

impl Transform {
    pub fn local_position(&self) -> Vec2 {
        self.local_position
    }

    pub fn local_rotation(&self) -> Angle {
        self.local_rotation
    }

    /// Этаж относительно родителя.
    pub fn level(&self) -> i16 {
        self.z
    }

    pub fn anchored(&self) -> bool {
        self.anchored
    }

    pub fn no_rot(&self) -> bool {
        self.no_rot
    }

    /// Сущность карты; `None` — сущность нигде не находится.
    pub fn map(&self) -> Option<Entity> {
        self.map
    }

    /// Грид, на котором стоит сущность; `None` — в открытом космосе карты.
    pub fn grid(&self) -> Option<Entity> {
        self.grid
    }
}

/// Сущность переместилась или сменила родителя. Поднимается на самой сущности.
#[derive(Debug, Clone, Copy)]
pub struct MoveEvent {
    pub entity: Entity,
    pub old_parent: Option<Entity>,
    pub new_parent: Option<Entity>,
    pub old_position: Vec2,
    pub new_position: Vec2,
    pub old_rotation: Angle,
    pub new_rotation: Angle,
    pub old_level: i16,
    pub new_level: i16,
}

impl MoveEvent {
    pub fn parent_changed(&self) -> bool {
        self.old_parent != self.new_parent
    }
}

/// У сущности сменился родитель (`EntParentChangedMessage` Robust).
#[derive(Debug, Clone, Copy)]
pub struct EntParentChanged {
    pub entity: Entity,
    pub old_parent: Option<Entity>,
    /// Карта до смены родителя.
    pub old_map: Option<Entity>,
}

/// Сущность привязана к клетке или отвязана от неё.
#[derive(Debug, Clone, Copy)]
pub struct AnchorStateChanged {
    pub entity: Entity,
    pub grid: Entity,
    pub tile: TileCoord,
    pub anchored: bool,
    /// Отвязка из-за удаления сущности или смены родителя.
    pub detaching: bool,
}

/// Положение в мировых координатах карты.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldTransform {
    pub position: Vec2,
    pub rotation: Angle,
    pub level: i16,
}

impl WorldTransform {
    /// Точка из координат этой сущности в мировые.
    pub fn to_world(&self, local: Vec2) -> Vec2 {
        self.rotation.rotate(local) + self.position
    }

    /// Точка из мировых координат в координаты этой сущности.
    pub fn to_local(&self, world: Vec2) -> Vec2 {
        (-self.rotation).rotate(world - self.position)
    }
}

/// Почему положение не изменено.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransformError {
    #[error("у сущности нет компонента Transform")]
    NoTransform,
    #[error("сущность удаляется")]
    Terminating,
    #[error("новый родитель не существует, удаляется или не имеет Transform")]
    BadParent,
    #[error("сущность нельзя сделать потомком самой себя или своего потомка")]
    Cycle,
}

/// Наибольшая глубина иерархии, которую обходят вычисления мировых координат.
const MAX_DEPTH: usize = 64;

fn parent_of(world: &World, entity: Entity) -> Option<Entity> {
    world.get::<ChildOf>(entity).map(ChildOf::parent)
}

fn children_of(world: &World, entity: Entity) -> SmallVec<[Entity; 8]> {
    world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default()
}

fn is_terminating(world: &World, entity: Entity) -> bool {
    world
        .get::<MetaData>(entity)
        .is_some_and(MetaData::terminating)
}

/// Обновляет карту и грид сущности и её потомков после смены родителя.
fn update_location(
    world: &mut World,
    entity: Entity,
    parent_map: Option<Entity>,
    parent_grid: Option<Entity>,
) {
    let is_map = world.get::<Map>(entity).is_some();
    let is_grid = world.get::<MapGrid>(entity).is_some();
    let Some(mut transform) = world.get_mut::<Transform>(entity) else {
        return;
    };
    let new_map = if is_map { Some(entity) } else { parent_map };
    let new_grid = if is_grid { Some(entity) } else { parent_grid };
    let old_map = transform.map;
    transform.map = new_map;
    transform.grid = new_grid;
    if old_map != new_map {
        if is_grid {
            if let Some(mut map) = old_map.and_then(|m| world.get_mut::<Map>(m)) {
                map.grids.retain(|g| *g != entity);
            }
            if let Some(mut map) = new_map.and_then(|m| world.get_mut::<Map>(m)) {
                map.grids.push(entity);
            }
        }
        // Сущность живёт по часам своей карты.
        let paused = new_map
            .and_then(|m| world.get::<Map>(m))
            .is_some_and(|map| map.paused);
        world.set_entity_paused(entity, paused);
    }
    for child in children_of(world, entity) {
        update_location(world, child, new_map, new_grid);
    }
}

/// Положение и иерархия на `World`.
pub trait TransformWorldExt {
    /// Создаёт сущность с метаданными и `Transform` в стадии `Spawned`.
    fn spawn_entity(&mut self) -> Entity;

    /// Родитель сущности в иерархии.
    fn parent(&self, entity: Entity) -> Option<Entity>;

    /// Положение в мировых координатах карты.
    fn world_transform(&self, entity: Entity) -> Option<WorldTransform>;

    /// Положение на карте; `None` — сущность нигде не находится.
    fn map_coordinates(&self, entity: Entity) -> Option<MapCoordinates>;

    /// Положение относительно родителя.
    fn coordinates(&self, entity: Entity) -> Option<EntityCoordinates>;

    /// Координаты относительно сущности в координаты карты.
    fn to_map_coordinates(&self, coordinates: EntityCoordinates) -> Option<MapCoordinates>;

    /// Задаёт родителя и положение относительно него; `rotation: None` сохраняет локальный
    /// поворот. Привязанная к клетке сущность отвязывается.
    fn set_coordinates(
        &mut self,
        entity: Entity,
        coordinates: EntityCoordinates,
        rotation: Option<Angle>,
    ) -> Result<(), TransformError>;

    /// Перемещает сущность внутри её родителя.
    fn set_local_position(&mut self, entity: Entity, position: Vec2) -> Result<(), TransformError>;

    /// Поворачивает сущность относительно родителя.
    fn set_local_rotation(&mut self, entity: Entity, rotation: Angle)
    -> Result<(), TransformError>;

    /// Меняет родителя, сохраняя мировое положение и поворот.
    fn set_parent(&mut self, entity: Entity, parent: Entity) -> Result<(), TransformError>;

    /// Перемещает сущность в мировую точку её карты, не меняя родителя.
    fn set_world_position(&mut self, entity: Entity, position: Vec2) -> Result<(), TransformError>;

    /// Убирает сущность из иерархии: она нигде не находится.
    fn detach_to_null(&mut self, entity: Entity);

    /// Привязывает сущность к клетке грида, на котором она стоит. `false` — сущность не
    /// стоит прямо на гриде.
    fn anchor(&mut self, entity: Entity) -> bool;

    /// Отвязывает сущность от клетки.
    fn unanchor(&mut self, entity: Entity);

    /// Сущность `ancestor` — предок `entity` в иерархии.
    fn is_ancestor(&self, ancestor: Entity, entity: Entity) -> bool;

    /// Задаёт положение целиком, как оно записано в источнике: сетевом состоянии или файле
    /// карты. `parent: None` убирает сущность из иерархии. Привязка к клетке приводится
    /// к `placement.anchored`.
    fn set_placement(&mut self, entity: Entity, placement: Placement)
    -> Result<(), TransformError>;
}

/// Полное положение сущности для [`TransformWorldExt::set_placement`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub parent: Option<Entity>,
    pub position: Vec2,
    pub rotation: Angle,
    pub level: i16,
    pub anchored: bool,
    pub no_rot: bool,
}

fn unanchor_inner(world: &mut World, entity: Entity, detaching: bool) {
    let Some(mut transform) = world.get_mut::<Transform>(entity) else {
        return;
    };
    // Привязка из прототипа ещё не записана в клетку: её запишет инициализация сущности
    // на новом месте.
    if !transform.anchor_registered {
        return;
    }
    transform.anchored = false;
    transform.anchor_registered = false;
    let tile = TileCoord::containing(transform.local_position, transform.z);
    let Some(grid) = transform.grid else {
        return;
    };
    if let Some(mut map_grid) = world.get_mut::<MapGrid>(grid) {
        map_grid.remove_anchored(tile, entity);
    }
    world.raise(
        entity,
        &mut AnchorStateChanged {
            entity,
            grid,
            tile,
            anchored: false,
            detaching,
        },
    );
}

impl TransformWorldExt for World {
    fn spawn_entity(&mut self) -> Entity {
        let entity = self.spawn_uninitialized();
        self.entity_mut(entity).insert(Transform::default());
        entity
    }

    fn parent(&self, entity: Entity) -> Option<Entity> {
        parent_of(self, entity)
    }

    fn world_transform(&self, entity: Entity) -> Option<WorldTransform> {
        let transform = self.get::<Transform>(entity)?;
        let mut result = WorldTransform {
            position: transform.local_position,
            rotation: transform.local_rotation,
            level: transform.z,
        };
        let mut current = parent_of(self, entity);
        for _ in 0..MAX_DEPTH {
            let Some(ancestor) = current else {
                break;
            };
            let transform = self.get::<Transform>(ancestor)?;
            result.position =
                transform.local_rotation.rotate(result.position) + transform.local_position;
            result.rotation = result.rotation + transform.local_rotation;
            result.level += transform.z;
            current = parent_of(self, ancestor);
        }
        Some(result)
    }

    fn map_coordinates(&self, entity: Entity) -> Option<MapCoordinates> {
        let map = self.get::<Transform>(entity)?.map?;
        let id = self.get::<Map>(map)?.id;
        let world = self.world_transform(entity)?;
        Some(MapCoordinates::on_level(id, world.position, world.level))
    }

    fn coordinates(&self, entity: Entity) -> Option<EntityCoordinates> {
        let transform = self.get::<Transform>(entity)?;
        Some(EntityCoordinates::on_level(
            parent_of(self, entity)?,
            transform.local_position,
            transform.z,
        ))
    }

    fn to_map_coordinates(&self, coordinates: EntityCoordinates) -> Option<MapCoordinates> {
        let map = self.get::<Transform>(coordinates.parent)?.map?;
        let id = self.get::<Map>(map)?.id;
        let parent = self.world_transform(coordinates.parent)?;
        Some(MapCoordinates::on_level(
            id,
            parent.to_world(coordinates.position),
            parent.level + coordinates.z,
        ))
    }

    fn set_coordinates(
        &mut self,
        entity: Entity,
        coordinates: EntityCoordinates,
        rotation: Option<Angle>,
    ) -> Result<(), TransformError> {
        let transform = self
            .get::<Transform>(entity)
            .ok_or(TransformError::NoTransform)?;
        let old_parent = parent_of(self, entity);
        let old_position = transform.local_position;
        let old_rotation = transform.local_rotation;
        let old_level = transform.z;
        let old_map = transform.map;
        let new_rotation = match rotation {
            _ if transform.no_rot => Angle::ZERO,
            Some(rotation) => rotation,
            None => old_rotation,
        };
        let parent_changed = old_parent != Some(coordinates.parent);
        if !parent_changed
            && old_position == coordinates.position
            && old_rotation == new_rotation
            && old_level == coordinates.z
        {
            return Ok(());
        }
        if parent_changed {
            if is_terminating(self, entity) {
                return Err(TransformError::Terminating);
            }
            if coordinates.parent == entity || self.is_ancestor(entity, coordinates.parent) {
                return Err(TransformError::Cycle);
            }
            if self.get::<Transform>(coordinates.parent).is_none()
                || is_terminating(self, coordinates.parent)
            {
                return Err(TransformError::BadParent);
            }
        }
        // Привязанная сущность стоит в центре клетки: перед перемещением она отвязывается.
        let moved = old_position != coordinates.position || old_level != coordinates.z;
        if parent_changed || moved {
            unanchor_inner(self, entity, parent_changed);
        }

        if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            transform.local_position = coordinates.position;
            transform.local_rotation = new_rotation;
            transform.z = coordinates.z;
        }
        if parent_changed {
            self.entity_mut(entity).insert(ChildOf(coordinates.parent));
            let (map, grid) = self
                .get::<Transform>(coordinates.parent)
                .map_or((None, None), |parent| (parent.map, parent.grid));
            update_location(self, entity, map, grid);
            self.raise(
                entity,
                &mut EntParentChanged {
                    entity,
                    old_parent,
                    old_map,
                },
            );
        }
        self.raise(
            entity,
            &mut MoveEvent {
                entity,
                old_parent,
                new_parent: Some(coordinates.parent),
                old_position,
                new_position: coordinates.position,
                old_rotation,
                new_rotation,
                old_level,
                new_level: coordinates.z,
            },
        );
        Ok(())
    }

    fn set_local_position(&mut self, entity: Entity, position: Vec2) -> Result<(), TransformError> {
        let level = self
            .get::<Transform>(entity)
            .ok_or(TransformError::NoTransform)?
            .z;
        let parent = parent_of(self, entity).ok_or(TransformError::BadParent)?;
        self.set_coordinates(
            entity,
            EntityCoordinates::on_level(parent, position, level),
            None,
        )
    }

    fn set_local_rotation(
        &mut self,
        entity: Entity,
        rotation: Angle,
    ) -> Result<(), TransformError> {
        let transform = self
            .get::<Transform>(entity)
            .ok_or(TransformError::NoTransform)?;
        let (position, level) = (transform.local_position, transform.z);
        match parent_of(self, entity) {
            Some(parent) => self.set_coordinates(
                entity,
                EntityCoordinates::on_level(parent, position, level),
                Some(rotation),
            ),
            // Корень иерархии (карта) поворачивается на месте.
            None => {
                if let Some(mut transform) = self.get_mut::<Transform>(entity)
                    && !transform.no_rot
                {
                    transform.local_rotation = rotation;
                }
                Ok(())
            }
        }
    }

    fn set_parent(&mut self, entity: Entity, parent: Entity) -> Result<(), TransformError> {
        let own = self
            .world_transform(entity)
            .ok_or(TransformError::NoTransform)?;
        let target = self
            .world_transform(parent)
            .ok_or(TransformError::BadParent)?;
        self.set_coordinates(
            entity,
            EntityCoordinates::on_level(
                parent,
                target.to_local(own.position),
                own.level - target.level,
            ),
            Some(own.rotation - target.rotation),
        )
    }

    fn set_world_position(&mut self, entity: Entity, position: Vec2) -> Result<(), TransformError> {
        let local = match parent_of(self, entity) {
            Some(parent) => self
                .world_transform(parent)
                .ok_or(TransformError::BadParent)?
                .to_local(position),
            None => position,
        };
        self.set_local_position(entity, local)
    }

    fn detach_to_null(&mut self, entity: Entity) {
        let Some(old_parent) = parent_of(self, entity) else {
            return;
        };
        unanchor_inner(self, entity, true);
        let Some(mut transform) = self.get_mut::<Transform>(entity) else {
            return;
        };
        let (old_position, old_rotation, old_level, old_map) = (
            transform.local_position,
            transform.local_rotation,
            transform.z,
            transform.map,
        );
        transform.local_position = Vec2::ZERO;
        transform.z = 0;
        self.entity_mut(entity).remove::<ChildOf>();
        update_location(self, entity, None, None);
        self.raise(
            entity,
            &mut EntParentChanged {
                entity,
                old_parent: Some(old_parent),
                old_map,
            },
        );
        self.raise(
            entity,
            &mut MoveEvent {
                entity,
                old_parent: Some(old_parent),
                new_parent: None,
                old_position,
                new_position: Vec2::ZERO,
                old_rotation,
                new_rotation: old_rotation,
                old_level,
                new_level: 0,
            },
        );
    }

    fn anchor(&mut self, entity: Entity) -> bool {
        let Some(transform) = self.get::<Transform>(entity) else {
            return false;
        };
        if transform.anchor_registered {
            return true;
        }
        let Some(grid) = transform
            .grid
            .filter(|g| parent_of(self, entity) == Some(*g))
        else {
            return false;
        };
        let old_position = transform.local_position;
        let (old_rotation, level) = (transform.local_rotation, transform.z);
        let tile = TileCoord::containing(old_position, level);
        let Some(mut map_grid) = self.get_mut::<MapGrid>(grid) else {
            return false;
        };
        map_grid.add_anchored(tile, entity);
        if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            transform.anchored = true;
            transform.anchor_registered = true;
            transform.local_position = tile.center();
        }
        self.raise(
            entity,
            &mut AnchorStateChanged {
                entity,
                grid,
                tile,
                anchored: true,
                detaching: false,
            },
        );
        if old_position != tile.center() {
            self.raise(
                entity,
                &mut MoveEvent {
                    entity,
                    old_parent: Some(grid),
                    new_parent: Some(grid),
                    old_position,
                    new_position: tile.center(),
                    old_rotation,
                    new_rotation: old_rotation,
                    old_level: level,
                    new_level: level,
                },
            );
        }
        true
    }

    fn unanchor(&mut self, entity: Entity) {
        unanchor_inner(self, entity, false);
        if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            transform.anchored = false;
        }
    }

    fn set_placement(
        &mut self,
        entity: Entity,
        placement: Placement,
    ) -> Result<(), TransformError> {
        let mut transform = self
            .get_mut::<Transform>(entity)
            .ok_or(TransformError::NoTransform)?;
        transform.no_rot = placement.no_rot;
        let initialized = self
            .get::<MetaData>(entity)
            .is_none_or(MetaData::initialized);
        match placement.parent {
            Some(parent) => self.set_coordinates(
                entity,
                EntityCoordinates::on_level(parent, placement.position, placement.level),
                Some(placement.rotation),
            )?,
            None => {
                self.detach_to_null(entity);
                if let Some(mut transform) = self.get_mut::<Transform>(entity) {
                    transform.local_position = placement.position;
                    transform.local_rotation = placement.rotation;
                    transform.z = placement.level;
                }
            }
        }
        if !placement.anchored {
            self.unanchor(entity);
        } else if initialized {
            self.anchor(entity);
        } else if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            // До инициализации привязку запишет `ComponentInit`.
            transform.anchored = true;
        }
        Ok(())
    }

    fn is_ancestor(&self, ancestor: Entity, entity: Entity) -> bool {
        let mut current = parent_of(self, entity);
        for _ in 0..MAX_DEPTH {
            match current {
                Some(found) if found == ancestor => return true,
                Some(found) => current = parent_of(self, found),
                None => return false,
            }
        }
        false
    }
}

/// Подписки жизненного цикла `Transform`.
pub(crate) fn register(world: &mut World) {
    use astrocore_ecs::{ComponentInit, EntityTerminating};

    // Прототип просит привязку (`anchored: true`): записываем её в клетку при инициализации.
    world.subscribe::<Transform, ComponentInit>(|world, entity, _| {
        let Some(transform) = world.get::<Transform>(entity) else {
            return;
        };
        if transform.anchored
            && !transform.anchor_registered
            && !world.anchor(entity)
            && let Some(mut transform) = world.get_mut::<Transform>(entity)
        {
            transform.anchored = false;
        }
    });
    world.subscribe::<Transform, EntityTerminating>(|world, entity, _| {
        unanchor_inner(world, entity, true);
    });
}
