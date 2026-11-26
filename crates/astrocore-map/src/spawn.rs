//! Создание сущностей в мире. Образец — `EntityManager.Spawn.cs` Robust @ 1afd692.

use astrocore_ecs::LifecycleWorldExt;
use astrocore_prototypes::{EntityOverrides, PrototypeWorldExt, SpawnError};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use crate::coords::{EntityCoordinates, MapCoordinates};
use crate::map::MapWorldExt;
use crate::transform::{Transform, TransformWorldExt};

/// Почему сущность не появилась в мире.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlaceError {
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    #[error("место появления недоступно: {0}")]
    Location(#[from] crate::transform::TransformError),
    #[error("карты с таким номером нет")]
    NoMap,
}

/// Создание сущностей из прототипов в заданном месте.
pub trait SpawnWorldExt {
    /// Создаёт сущность из прототипа относительно родителя (`SpawnAttachedTo` Robust):
    /// инициализирует её и, если карта инициализирована, поднимает `MapInit`.
    fn spawn_attached(
        &mut self,
        prototype: &str,
        coordinates: EntityCoordinates,
    ) -> Result<Entity, PlaceError>;

    /// То же с изменениями компонентов из файла карты.
    fn spawn_attached_with(
        &mut self,
        prototype: &str,
        coordinates: EntityCoordinates,
        overrides: &EntityOverrides<'_>,
    ) -> Result<Entity, PlaceError>;

    /// Создаёт сущность в точке карты: на гриде под точкой или в космосе (`Spawn` Robust).
    fn spawn_on_map(
        &mut self,
        prototype: &str,
        coordinates: MapCoordinates,
    ) -> Result<Entity, PlaceError>;
}

impl SpawnWorldExt for World {
    fn spawn_attached(
        &mut self,
        prototype: &str,
        coordinates: EntityCoordinates,
    ) -> Result<Entity, PlaceError> {
        self.spawn_attached_with(prototype, coordinates, &EntityOverrides::default())
    }

    fn spawn_attached_with(
        &mut self,
        prototype: &str,
        coordinates: EntityCoordinates,
        overrides: &EntityOverrides<'_>,
    ) -> Result<Entity, PlaceError> {
        let entity = self.spawn_prototype_with(prototype, overrides)?;
        if self.get::<Transform>(entity).is_none() {
            self.entity_mut(entity).insert(Transform::default());
        }
        let rotation = self.get::<Transform>(entity).map(Transform::local_rotation);
        if let Err(error) = self.set_coordinates(entity, coordinates, rotation) {
            self.delete_entity(entity);
            return Err(error.into());
        }
        let map_init = self.is_on_initialized_map(entity);
        self.initialize_and_start(entity, map_init);
        Ok(entity)
    }

    fn spawn_on_map(
        &mut self,
        prototype: &str,
        coordinates: MapCoordinates,
    ) -> Result<Entity, PlaceError> {
        let place = self
            .snap_to_grid_or_map(coordinates)
            .ok_or(PlaceError::NoMap)?;
        self.spawn_attached(prototype, place)
    }
}
