//! Пространство Astrocore: координаты, иерархия `Transform`, карты, гриды с этажами, тайлы
//! и контейнеры. Образец — `Robust.Shared/Map`, `SharedTransformSystem`, `SharedMapSystem`
//! и `Robust.Shared/Containers`; этажи Z встроены в координаты и гриды (ADR-0015).

mod container;
mod coords;
mod grid;
mod loader;
mod map;
mod saver;
mod spawn;
mod transform;
mod traversal;

use astrocore_ecs::{GamePlugin, RegistryWorldExt};
use bevy_ecs::world::World;

pub use container::{
    CONTAINER_COMPONENT, Container, ContainerError, ContainerGettingInsertedAttempt,
    ContainerGettingRemovedAttempt, ContainerIsInsertingAttempt, ContainerIsRemovingAttempt,
    ContainerKind, ContainerManager, ContainerWorldExt, EntGotInsertedIntoContainer,
    EntGotRemovedFromContainer, EntInsertedIntoContainer, EntRemovedFromContainer, InContainer,
};
pub use coords::{
    CHUNK_SIZE, CHUNK_TILES, ChunkPos, EntityCoordinates, MapCoordinates, MapId, TileCoord,
};
pub use grid::{
    Chunk, MapGrid, SPACE_TILE, TILE_SIZE, Tile, TileChange, TileChanged, TileDefinition,
    TileDefinitions, TileRef,
};
pub use loader::{
    EntityMigrations, MAP_FORMAT_RANGE, MapFileEntity, MapFileTilemap, MapLoadError,
    MapLoadOptions, MapLoadReport, MapLoadWorldExt, MapPreserved,
};
pub use map::{Map, MapCreated, MapError, MapRemoved, MapWorldExt, Maps};
pub use saver::{MAP_FORMAT_VERSION, MapSave, MapSaveError, MapSaveOptions, MapSaveWorldExt};
pub use spawn::{PlaceError, SpawnWorldExt};
pub use transform::{
    AnchorStateChanged, EntParentChanged, MoveEvent, Placement, Transform, TransformError,
    TransformWorldExt, WorldTransform,
};
pub use traversal::GridTraversal;

/// Подключает пространство к миру: компоненты данных, ресурсы и подписки.
pub struct MapPlugin;

impl GamePlugin for MapPlugin {
    fn build(&self, world: &mut World) {
        world.register_data_component::<Transform>();
        world.init_resource::<TileDefinitions>();
        transform::register(world);
        map::register(world);
        container::register(world);
        traversal::register(world);
    }
}

#[cfg(test)]
mod tests;
