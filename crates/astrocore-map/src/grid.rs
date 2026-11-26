//! Грид: тайловая сетка с этажами. Образец — `MapGridComponent` и `SharedMapSystem.Grid.cs`
//! Robust @ 1afd692. Отличие — этаж в ключе чанка: один грид хранит все палубы станции.

use std::collections::{BTreeMap, HashMap};

use astrocore_ecs::GameTick;
use astrocore_math::Box2;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::coords::{CHUNK_SIZE, CHUNK_TILES, ChunkPos, TileCoord};

/// Тайл: номер определения и вариант рисунка. Номер 0 — пустота (космос).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Tile {
    /// Номер определения в [`TileDefinitions`].
    pub type_id: u16,
    /// Вариант рисунка.
    pub variant: u8,
    /// Флаги игры.
    pub flags: u8,
    /// Поворот и отражение рисунка.
    pub rotation: u8,
}

impl Tile {
    pub const EMPTY: Tile = Tile {
        type_id: 0,
        variant: 0,
        flags: 0,
        rotation: 0,
    };

    pub const fn new(type_id: u16) -> Self {
        Self {
            type_id,
            variant: 0,
            flags: 0,
            rotation: 0,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.type_id == 0
    }
}

/// Тайл вместе с его местом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRef {
    pub grid: Entity,
    pub coord: TileCoord,
    pub tile: Tile,
}

/// Изменение одного тайла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileChange {
    pub coord: TileCoord,
    pub old: Tile,
    pub new: Tile,
}

/// Тайлы грида изменились. Поднимается на гриде и широковещательно.
#[derive(Debug, Clone)]
pub struct TileChanged {
    pub grid: Entity,
    pub changes: Vec<TileChange>,
}

/// Чанк: [`CHUNK_SIZE`]×[`CHUNK_SIZE`] тайлов одного этажа.
#[derive(Debug, Clone)]
pub struct Chunk {
    tiles: Box<[Tile; CHUNK_TILES]>,
    filled: u16,
    /// Номер клетки в чанке → привязанные к ней сущности.
    anchored: HashMap<u16, SmallVec<[Entity; 2]>>,
    /// Тик последнего изменения тайлов чанка.
    pub last_modified: GameTick,
}

impl Chunk {
    fn new() -> Self {
        Self {
            tiles: Box::new([Tile::EMPTY; CHUNK_TILES]),
            filled: 0,
            anchored: HashMap::new(),
            last_modified: GameTick::ZERO,
        }
    }

    /// Тайлы чанка: строки по `y`, в строке — по `x`.
    pub fn tiles(&self) -> &[Tile; CHUNK_TILES] {
        &self.tiles
    }

    /// Число непустых тайлов.
    pub fn filled(&self) -> u16 {
        self.filled
    }
}

/// Границы занятых тайлов этажа в клетках: включительно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LevelBounds {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

/// Грид: чанки тайлов по этажам и сущности, привязанные к клеткам.
#[derive(Component, Debug, Clone, Default)]
pub struct MapGrid {
    chunks: HashMap<ChunkPos, Chunk>,
    /// Этаж → число чанков и границы; `None` — границы надо пересчитать.
    levels: BTreeMap<i16, (u32, Option<LevelBounds>)>,
    tile_count: u32,
}

impl MapGrid {
    pub fn new() -> Self {
        Self::default()
    }

    /// Тайл клетки; вне чанков — пустота.
    pub fn tile(&self, coord: TileCoord) -> Tile {
        self.chunks
            .get(&coord.chunk())
            .map_or(Tile::EMPTY, |chunk| chunk.tiles[coord.index_in_chunk()])
    }

    /// Заменяет тайл без событий и возвращает прежний.
    pub(crate) fn set_tile_raw(&mut self, coord: TileCoord, tile: Tile, tick: GameTick) -> Tile {
        let position = coord.chunk();
        if tile.is_empty() && !self.chunks.contains_key(&position) {
            return Tile::EMPTY;
        }
        let chunk = self.chunks.entry(position).or_insert_with(|| {
            self.levels.entry(position.z).or_insert((0, None)).0 += 1;
            Chunk::new()
        });
        let old = std::mem::replace(&mut chunk.tiles[coord.index_in_chunk()], tile);
        if old == tile {
            return old;
        }
        chunk.last_modified = tick;
        match (old.is_empty(), tile.is_empty()) {
            (true, false) => {
                chunk.filled += 1;
                self.tile_count += 1;
                // Новый тайл границы только расширяет.
                if let Some((_, Some(bounds))) = self.levels.get_mut(&coord.z) {
                    bounds.min_x = bounds.min_x.min(coord.x);
                    bounds.min_y = bounds.min_y.min(coord.y);
                    bounds.max_x = bounds.max_x.max(coord.x);
                    bounds.max_y = bounds.max_y.max(coord.y);
                } else if let Some(level) = self.levels.get_mut(&coord.z) {
                    level.1 = None;
                }
            }
            (false, true) => {
                chunk.filled -= 1;
                self.tile_count -= 1;
                let remove_chunk = chunk.filled == 0 && chunk.anchored.is_empty();
                if remove_chunk {
                    self.chunks.remove(&position);
                }
                if let Some(level) = self.levels.get_mut(&coord.z) {
                    level.1 = None;
                    if remove_chunk {
                        level.0 -= 1;
                        if level.0 == 0 {
                            self.levels.remove(&coord.z);
                        }
                    }
                }
            }
            _ => {}
        }
        old
    }

    /// Число непустых тайлов на всех этажах.
    pub fn tile_count(&self) -> u32 {
        self.tile_count
    }

    pub fn is_empty(&self) -> bool {
        self.tile_count == 0
    }

    /// Этажи, на которых есть чанки, снизу вверх.
    pub fn levels(&self) -> impl DoubleEndedIterator<Item = i16> + '_ {
        self.levels.keys().copied()
    }

    pub fn chunk(&self, position: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&position)
    }

    pub fn chunks(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        self.chunks
            .iter()
            .map(|(position, chunk)| (*position, chunk))
    }

    /// Непустые тайлы этажа.
    pub fn tiles_on_level(&self, z: i16) -> impl Iterator<Item = (TileCoord, Tile)> + '_ {
        self.chunks
            .iter()
            .filter(move |(position, _)| position.z == z)
            .flat_map(|(position, chunk)| {
                chunk
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, tile)| !tile.is_empty())
                    .map(|(index, tile)| (position.tile(index), *tile))
            })
    }

    /// Границы занятых тайлов этажа в координатах грида; `None` — этаж пуст.
    pub fn local_bounds(&mut self, z: i16) -> Option<Box2> {
        let level = self.levels.get(&z)?;
        let bounds = match level.1 {
            Some(bounds) => bounds,
            None => {
                let mut found: Option<LevelBounds> = None;
                for (coord, _) in self.tiles_on_level(z) {
                    let bounds = found.get_or_insert(LevelBounds {
                        min_x: coord.x,
                        min_y: coord.y,
                        max_x: coord.x,
                        max_y: coord.y,
                    });
                    bounds.min_x = bounds.min_x.min(coord.x);
                    bounds.min_y = bounds.min_y.min(coord.y);
                    bounds.max_x = bounds.max_x.max(coord.x);
                    bounds.max_y = bounds.max_y.max(coord.y);
                }
                let bounds = found?;
                if let Some(level) = self.levels.get_mut(&z) {
                    level.1 = Some(bounds);
                }
                bounds
            }
        };
        Some(Box2::new(
            bounds.min_x as f32,
            bounds.min_y as f32,
            (bounds.max_x + 1) as f32,
            (bounds.max_y + 1) as f32,
        ))
    }

    /// Сущности, привязанные к клетке.
    pub fn anchored_at(&self, coord: TileCoord) -> &[Entity] {
        self.chunks
            .get(&coord.chunk())
            .and_then(|chunk| chunk.anchored.get(&(coord.index_in_chunk() as u16)))
            .map_or(&[], |entities| entities.as_slice())
    }

    pub(crate) fn add_anchored(&mut self, coord: TileCoord, entity: Entity) {
        let position = coord.chunk();
        let chunk = self.chunks.entry(position).or_insert_with(|| {
            self.levels.entry(position.z).or_insert((0, None)).0 += 1;
            Chunk::new()
        });
        let cell = chunk
            .anchored
            .entry(coord.index_in_chunk() as u16)
            .or_default();
        if !cell.contains(&entity) {
            cell.push(entity);
        }
    }

    pub(crate) fn remove_anchored(&mut self, coord: TileCoord, entity: Entity) {
        let position = coord.chunk();
        let Some(chunk) = self.chunks.get_mut(&position) else {
            return;
        };
        let index = coord.index_in_chunk() as u16;
        if let Some(cell) = chunk.anchored.get_mut(&index) {
            cell.retain(|e| *e != entity);
            if cell.is_empty() {
                chunk.anchored.remove(&index);
            }
        }
        if chunk.filled == 0 && chunk.anchored.is_empty() {
            self.chunks.remove(&position);
            if let Some(level) = self.levels.get_mut(&position.z) {
                level.0 -= 1;
                if level.0 == 0 {
                    self.levels.remove(&position.z);
                }
            }
        }
    }
}

/// Сторона тайла в метрах (как `EyeManager.PixelsPerMeter`/`TileSize` Robust: 1 тайл = 1 м).
pub const TILE_SIZE: f32 = 1.0;

/// Определение тайла, известное движку. Игровые свойства (трение, звук шагов, предметы
/// при разборке) хранит прототип игры.
#[derive(Debug, Clone, PartialEq)]
pub struct TileDefinition {
    /// Идентификатор прототипа, например `FloorSteel`.
    pub id: String,
    /// Номер в сетке: значение [`Tile::type_id`].
    pub type_id: u16,
    /// Путь к рисунку относительно каталога ресурсов.
    pub sprite: Option<String>,
    /// Число вариантов рисунка.
    pub variants: u8,
}

/// Определения тайлов: идентификатор прототипа ↔ номер в сетке
/// (`ITileDefinitionManager` Robust). Номер 0 — пустота `Space`. Ресурс `World`.
#[derive(Resource, Debug, Clone)]
pub struct TileDefinitions {
    list: Vec<TileDefinition>,
    by_id: HashMap<String, u16>,
}

/// Идентификатор пустого тайла.
pub const SPACE_TILE: &str = "Space";

impl Default for TileDefinitions {
    fn default() -> Self {
        let mut definitions = Self {
            list: Vec::new(),
            by_id: HashMap::new(),
        };
        definitions.register(SPACE_TILE, None, 1);
        definitions
    }
}

impl TileDefinitions {
    /// Регистрирует определение и возвращает его номер; повторная регистрация обновляет
    /// рисунок и сохраняет номер.
    pub fn register(&mut self, id: &str, sprite: Option<String>, variants: u8) -> u16 {
        if let Some(&type_id) = self.by_id.get(id) {
            let definition = &mut self.list[usize::from(type_id)];
            definition.sprite = sprite;
            definition.variants = variants.max(1);
            return type_id;
        }
        let type_id = u16::try_from(self.list.len()).expect("слишком много определений тайлов");
        self.list.push(TileDefinition {
            id: id.to_owned(),
            type_id,
            sprite,
            variants: variants.max(1),
        });
        self.by_id.insert(id.to_owned(), type_id);
        type_id
    }

    pub fn get(&self, type_id: u16) -> Option<&TileDefinition> {
        self.list.get(usize::from(type_id))
    }

    pub fn by_id(&self, id: &str) -> Option<&TileDefinition> {
        self.by_id
            .get(id)
            .and_then(|&type_id| self.list.get(usize::from(type_id)))
    }

    /// Тайл по идентификатору определения.
    pub fn tile(&self, id: &str) -> Option<Tile> {
        self.by_id.get(id).map(|&type_id| Tile::new(type_id))
    }

    pub fn iter(&self) -> impl Iterator<Item = &TileDefinition> {
        self.list.iter()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Проверка того, что сторона чанка укладывается в индекс привязок.
const _: () = assert!(CHUNK_SIZE * CHUNK_SIZE <= u16::MAX as i32);

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: Tile = Tile::new(1);

    #[test]
    fn tiles_chunks_levels_and_bounds() {
        let mut grid = MapGrid::new();
        assert!(grid.is_empty() && grid.local_bounds(0).is_none());
        assert_eq!(
            grid.set_tile_raw(TileCoord::new(5, 5, 0), Tile::EMPTY, GameTick(1)),
            Tile::EMPTY
        );
        assert_eq!(grid.chunks().count(), 0);

        grid.set_tile_raw(TileCoord::new(-1, -1, 0), FLOOR, GameTick(1));
        grid.set_tile_raw(TileCoord::new(20, 3, 0), FLOOR, GameTick(2));
        grid.set_tile_raw(TileCoord::new(0, 0, 1), FLOOR, GameTick(3));
        assert_eq!(grid.tile_count(), 3);
        assert_eq!(grid.levels().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(grid.chunks().count(), 3);
        assert_eq!(grid.local_bounds(0), Some(Box2::new(-1.0, -1.0, 21.0, 4.0)));
        assert_eq!(grid.local_bounds(1), Some(Box2::new(0.0, 0.0, 1.0, 1.0)));
        assert_eq!(grid.tile(TileCoord::new(20, 3, 0)), FLOOR);
        assert_eq!(grid.tile(TileCoord::new(20, 3, 1)), Tile::EMPTY);
        assert_eq!(
            grid.chunk(TileCoord::new(20, 3, 0).chunk())
                .unwrap()
                .last_modified,
            GameTick(2)
        );

        // Границы расширяются без пересчёта и сужаются после удаления.
        grid.set_tile_raw(TileCoord::new(30, 3, 0), FLOOR, GameTick(4));
        assert_eq!(grid.local_bounds(0), Some(Box2::new(-1.0, -1.0, 31.0, 4.0)));
        grid.set_tile_raw(TileCoord::new(30, 3, 0), Tile::EMPTY, GameTick(5));
        grid.set_tile_raw(TileCoord::new(20, 3, 0), Tile::EMPTY, GameTick(5));
        assert_eq!(grid.local_bounds(0), Some(Box2::new(-1.0, -1.0, 0.0, 0.0)));
        grid.set_tile_raw(TileCoord::new(0, 0, 1), Tile::EMPTY, GameTick(6));
        assert_eq!(grid.levels().collect::<Vec<_>>(), [0]);
        assert_eq!(grid.tiles_on_level(0).count(), 1);
    }

    #[test]
    fn anchored_entities_keep_chunk_alive() {
        let mut grid = MapGrid::new();
        let coord = TileCoord::new(3, 4, 0);
        let (a, b) = (
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
        );
        grid.set_tile_raw(coord, FLOOR, GameTick(1));
        grid.add_anchored(coord, a);
        grid.add_anchored(coord, a);
        grid.add_anchored(coord, b);
        assert_eq!(grid.anchored_at(coord), [a, b]);
        grid.set_tile_raw(coord, Tile::EMPTY, GameTick(2));
        assert_eq!(grid.anchored_at(coord), [a, b]);
        grid.remove_anchored(coord, a);
        grid.remove_anchored(coord, b);
        assert!(grid.anchored_at(coord).is_empty());
        assert_eq!(grid.chunks().count(), 0);
        assert_eq!(grid.levels().count(), 0);
    }

    #[test]
    fn tile_definitions_keep_ids_stable() {
        let mut definitions = TileDefinitions::default();
        assert_eq!(definitions.by_id(SPACE_TILE).unwrap().type_id, 0);
        let steel = definitions.register("FloorSteel", Some("Tiles/steel.png".into()), 4);
        assert_eq!(steel, 1);
        assert_eq!(definitions.register("FloorSteel", None, 0), 1);
        assert_eq!(definitions.get(1).unwrap().variants, 1);
        assert_eq!(definitions.tile("FloorSteel"), Some(Tile::new(1)));
        assert!(definitions.tile("Missing").is_none());
        assert_eq!(definitions.len(), 2);
    }
}
