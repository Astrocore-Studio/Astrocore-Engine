//! Карты и операции с гридами. Образец — `SharedMapSystem.Map.cs`, `.Pause.cs`,
//! `.MapInit.cs` и `.Grid.cs` Robust @ 1afd692.
//!
//! Карта — корень иерархии. Новая карта для маппинга создаётся неинициализированной и на
//! паузе: её сущности не тикают и не получают `MapInit`, поэтому в файл карты попадает
//! авторский контент, а не то, что наиграла игра.

use std::collections::HashMap;

use astrocore_ecs::{EventWorldExt, GameTime, LifecycleWorldExt, MetaData};
use astrocore_math::Vec2;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use smallvec::SmallVec;

use crate::coords::{EntityCoordinates, MapCoordinates, MapId, TileCoord};
use crate::grid::{MapGrid, Tile, TileChange, TileChanged, TileRef};
use crate::transform::{Transform, TransformWorldExt};

/// Карта: корень иерархии сущностей.
#[derive(Component, Debug, Clone)]
pub struct Map {
    pub id: MapId,
    /// Сущности карты получили `MapInit`; новые получают его при появлении.
    pub initialized: bool,
    /// Сущности карты не тикают.
    pub paused: bool,
    /// Гриды карты.
    pub(crate) grids: Vec<Entity>,
}

impl Map {
    /// Гриды карты.
    pub fn grids(&self) -> &[Entity] {
        &self.grids
    }
}

/// Карты мира по номерам. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct Maps {
    by_id: HashMap<MapId, Entity>,
    last_id: u32,
}

impl Maps {
    pub fn entity(&self, id: MapId) -> Option<Entity> {
        self.by_id.get(&id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (MapId, Entity)> + '_ {
        self.by_id.iter().map(|(id, entity)| (*id, *entity))
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Карта создана. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct MapCreated {
    pub map: Entity,
    pub id: MapId,
}

/// Карта удаляется. Широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct MapRemoved {
    pub map: Entity,
    pub id: MapId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("карта {} уже существует", .0.0)]
    Exists(MapId),
    #[error("номер карты 0 занят под «нигде»")]
    Nullspace,
}

fn descendants(world: &World, root: Entity) -> Vec<Entity> {
    let mut found = vec![root];
    let mut index = 0;
    while index < found.len() {
        if let Some(children) = world.get::<Children>(found[index]) {
            found.extend(children.iter().copied());
        }
        index += 1;
    }
    found
}

/// Карты и гриды на `World`.
pub trait MapWorldExt {
    /// Создаёт карту со следующим свободным номером. `initialized: false` — карта для
    /// маппинга: на паузе и без `MapInit`.
    fn create_map(&mut self, initialized: bool) -> (Entity, MapId);

    /// Создаёт карту с заданным номером.
    fn create_map_with_id(&mut self, id: MapId, initialized: bool) -> Result<Entity, MapError>;

    /// Делает существующую сущность картой с заданным номером и флагами: так клиент
    /// принимает карту из сетевого состояния. Повторный вызов обновляет флаги.
    fn adopt_map(&mut self, entity: Entity, id: MapId, initialized: bool, paused: bool);

    /// Сущность карты по номеру.
    fn map_entity(&self, id: MapId) -> Option<Entity>;

    /// Номер карты, на которой находится сущность.
    fn map_id_of(&self, entity: Entity) -> Option<MapId>;

    /// Карта сущности инициализирована. Сущности «нигде» считаются на инициализированной
    /// карте, как в Robust.
    fn is_on_initialized_map(&self, entity: Entity) -> bool;

    /// Ставит карту и все её сущности на паузу или снимает с неё.
    fn set_map_paused(&mut self, map: Entity, paused: bool);

    /// Инициализирует карту: все её сущности получают `MapInit`, пауза снимается.
    fn initialize_map(&mut self, map: Entity);

    /// Создаёт пустой грид в начале координат карты.
    fn create_grid(&mut self, map: Entity) -> Entity;

    /// Делает существующую сущность гридом (сетевое состояние, загрузка карты) и вносит её
    /// в список гридов её карты. Повторный вызов безвреден.
    fn ensure_grid(&mut self, entity: Entity);

    /// Меняет тайл и поднимает [`TileChanged`]. `false` — у сущности нет грида или тайл
    /// не изменился.
    fn set_tile(&mut self, grid: Entity, coord: TileCoord, tile: Tile) -> bool;

    /// Меняет несколько тайлов одним событием; возвращает число изменённых.
    fn set_tiles(
        &mut self,
        grid: Entity,
        tiles: impl IntoIterator<Item = (TileCoord, Tile)>,
    ) -> usize;

    /// Грид с непустым тайлом в точке карты.
    fn find_grid_at(&self, coordinates: MapCoordinates) -> Option<Entity>;

    /// Тайл в точке карты.
    fn tile_at(&self, coordinates: MapCoordinates) -> Option<TileRef>;

    /// Клетка грида, в которой стоит сущность.
    fn tile_of(&self, entity: Entity) -> Option<TileRef>;

    /// Координаты для размещения в точке карты: на гриде под точкой или в космосе карты.
    fn snap_to_grid_or_map(&self, coordinates: MapCoordinates) -> Option<EntityCoordinates>;
}

impl MapWorldExt for World {
    fn create_map(&mut self, initialized: bool) -> (Entity, MapId) {
        let maps = self.get_resource_or_insert_with(Maps::default);
        let mut id = maps.last_id + 1;
        while maps.by_id.contains_key(&MapId(id)) {
            id += 1;
        }
        let id = MapId(id);
        let map = self
            .create_map_with_id(id, initialized)
            .expect("номер карты свободен");
        (map, id)
    }

    fn create_map_with_id(&mut self, id: MapId, initialized: bool) -> Result<Entity, MapError> {
        if id == MapId(0) {
            return Err(MapError::Nullspace);
        }
        let mut maps = self.get_resource_or_insert_with(Maps::default);
        if maps.by_id.contains_key(&id) {
            return Err(MapError::Exists(id));
        }
        maps.last_id = maps.last_id.max(id.0);
        let map = self.spawn_entity();
        self.resource_mut::<Maps>().by_id.insert(id, map);
        if let Some(mut meta) = self.get_mut::<MetaData>(map) {
            meta.name = Some(format!("map {}", id.0));
        }
        if let Some(mut transform) = self.get_mut::<Transform>(map) {
            transform.map = Some(map);
        }
        self.add_comp(
            map,
            Map {
                id,
                initialized,
                paused: !initialized,
                grids: Vec::new(),
            },
        );
        self.initialize_and_start(map, initialized);
        if !initialized {
            self.set_entity_paused(map, true);
        }
        self.raise_broadcast(&mut MapCreated { map, id });
        Ok(map)
    }

    fn adopt_map(&mut self, entity: Entity, id: MapId, initialized: bool, paused: bool) {
        if let Some(mut map) = self.get_mut::<Map>(entity) {
            map.initialized = initialized;
            map.paused = paused;
            return;
        }
        let mut maps = self.get_resource_or_insert_with(Maps::default);
        maps.last_id = maps.last_id.max(id.0);
        maps.by_id.insert(id, entity);
        if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            transform.map = Some(entity);
        }
        self.add_comp(
            entity,
            Map {
                id,
                initialized,
                paused,
                grids: Vec::new(),
            },
        );
    }

    fn map_entity(&self, id: MapId) -> Option<Entity> {
        self.get_resource::<Maps>()?.entity(id)
    }

    fn map_id_of(&self, entity: Entity) -> Option<MapId> {
        let map = self.get::<Transform>(entity)?.map?;
        self.get::<Map>(map).map(|map| map.id)
    }

    fn is_on_initialized_map(&self, entity: Entity) -> bool {
        self.get::<Transform>(entity)
            .and_then(|transform| transform.map)
            .and_then(|map| self.get::<Map>(map))
            .is_none_or(|map| map.initialized)
    }

    fn set_map_paused(&mut self, map: Entity, paused: bool) {
        let Some(mut state) = self.get_mut::<Map>(map) else {
            return;
        };
        if state.paused == paused {
            return;
        }
        state.paused = paused;
        for entity in descendants(self, map) {
            self.set_entity_paused(entity, paused);
        }
    }

    fn initialize_map(&mut self, map: Entity) {
        let Some(mut state) = self.get_mut::<Map>(map) else {
            return;
        };
        if state.initialized {
            return;
        }
        state.initialized = true;
        for entity in descendants(self, map) {
            self.run_map_init(entity);
        }
        self.set_map_paused(map, false);
    }

    fn create_grid(&mut self, map: Entity) -> Entity {
        let grid = self.spawn_entity();
        if let Some(mut meta) = self.get_mut::<MetaData>(grid) {
            meta.name = Some("grid".to_owned());
        }
        self.add_comp(grid, MapGrid::new());
        // Родитель задан до инициализации: грид сразу попадает в список гридов карты.
        let _ = self.set_coordinates(grid, EntityCoordinates::new(map, Vec2::ZERO), None);
        let initialized = self.is_on_initialized_map(grid);
        self.initialize_and_start(grid, initialized);
        grid
    }

    fn ensure_grid(&mut self, entity: Entity) {
        if self.get::<MapGrid>(entity).is_none() {
            self.add_comp(entity, MapGrid::new());
        }
        if let Some(mut transform) = self.get_mut::<Transform>(entity) {
            transform.grid = Some(entity);
            if let Some(map) = transform.map
                && let Some(mut map) = self.get_mut::<Map>(map)
                && !map.grids.contains(&entity)
            {
                map.grids.push(entity);
            }
        }
    }

    fn set_tile(&mut self, grid: Entity, coord: TileCoord, tile: Tile) -> bool {
        self.set_tiles(grid, [(coord, tile)]) > 0
    }

    fn set_tiles(
        &mut self,
        grid: Entity,
        tiles: impl IntoIterator<Item = (TileCoord, Tile)>,
    ) -> usize {
        let tick = self
            .get_resource::<GameTime>()
            .map(|time| time.tick)
            .unwrap_or_default();
        let Some(mut map_grid) = self.get_mut::<MapGrid>(grid) else {
            return 0;
        };
        let mut changes = Vec::new();
        for (coord, new) in tiles {
            let old = map_grid.set_tile_raw(coord, new, tick);
            if old != new {
                changes.push(TileChange { coord, old, new });
            }
        }
        let count = changes.len();
        if count > 0 {
            self.raise_and_broadcast(grid, &mut TileChanged { grid, changes });
        }
        count
    }

    fn find_grid_at(&self, coordinates: MapCoordinates) -> Option<Entity> {
        self.tile_at(coordinates).map(|tile| tile.grid)
    }

    fn tile_at(&self, coordinates: MapCoordinates) -> Option<TileRef> {
        let map = self.map_entity(coordinates.map)?;
        let grids: SmallVec<[Entity; 8]> = self.get::<Map>(map)?.grids.iter().copied().collect();
        for grid in grids {
            let (Some(placement), Some(map_grid)) =
                (self.world_transform(grid), self.get::<MapGrid>(grid))
            else {
                continue;
            };
            let coord = TileCoord::containing(
                placement.to_local(coordinates.position),
                coordinates.z - placement.level,
            );
            let tile = map_grid.tile(coord);
            if !tile.is_empty() {
                return Some(TileRef { grid, coord, tile });
            }
        }
        None
    }

    fn tile_of(&self, entity: Entity) -> Option<TileRef> {
        let transform = self.get::<Transform>(entity)?;
        let grid = transform.grid?;
        let map_grid = self.get::<MapGrid>(grid)?;
        // Сущность прямо на гриде: её локальные координаты — координаты грида.
        let coord = if self.parent(entity) == Some(grid) {
            TileCoord::containing(transform.local_position, transform.z)
        } else {
            let own = self.world_transform(entity)?;
            let placement = self.world_transform(grid)?;
            TileCoord::containing(
                placement.to_local(own.position),
                own.level - placement.level,
            )
        };
        Some(TileRef {
            grid,
            coord,
            tile: map_grid.tile(coord),
        })
    }

    fn snap_to_grid_or_map(&self, coordinates: MapCoordinates) -> Option<EntityCoordinates> {
        if let Some(tile) = self.tile_at(coordinates) {
            let placement = self.world_transform(tile.grid)?;
            return Some(EntityCoordinates::on_level(
                tile.grid,
                placement.to_local(coordinates.position),
                coordinates.z - placement.level,
            ));
        }
        let map = self.map_entity(coordinates.map)?;
        Some(EntityCoordinates::on_level(
            map,
            coordinates.position,
            coordinates.z,
        ))
    }
}

/// Подписки жизненного цикла карт и гридов.
pub(crate) fn register(world: &mut World) {
    use astrocore_ecs::EntityTerminating;

    world.init_resource::<Maps>();
    world.subscribe::<Map, EntityTerminating>(|world, map, _| {
        let Some(id) = world.get::<Map>(map).map(|m| m.id) else {
            return;
        };
        world.raise_broadcast(&mut MapRemoved { map, id });
        if let Some(mut maps) = world.get_resource_mut::<Maps>() {
            maps.by_id.remove(&id);
        }
    });
    world.subscribe::<MapGrid, EntityTerminating>(|world, grid, _| {
        let map = world.get::<Transform>(grid).and_then(|t| t.map);
        if let Some(mut map) = map.and_then(|m| world.get_mut::<Map>(m)) {
            map.grids.retain(|g| *g != grid);
        }
    });
}
