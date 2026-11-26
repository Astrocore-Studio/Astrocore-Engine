//! Координаты. Образец — `Robust.Shared/Map` (`MapId`, `MapCoordinates`, `EntityCoordinates`)
//! @ 1afd692, с этажом `z` во всех видах координат (ADR-0015).

use astrocore_math::{IVec2, Vec2};
use bevy_ecs::entity::Entity;
use serde::{Deserialize, Serialize};

/// Сторона чанка грида в тайлах.
pub const CHUNK_SIZE: i32 = 16;
/// Тайлов в чанке.
pub const CHUNK_TILES: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;

/// Номер карты. Карты нумеруются с 1; сущность без карты находится «нигде».
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MapId(pub u32);

/// Клетка грида: целые `x`, `y` и этаж `z`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct TileCoord {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub z: i16,
}

impl TileCoord {
    pub const fn new(x: i32, y: i32, z: i16) -> Self {
        Self { x, y, z }
    }

    /// Клетка, в которую попадает точка в координатах грида.
    pub fn containing(position: Vec2, z: i16) -> Self {
        Self::new(position.x.floor() as i32, position.y.floor() as i32, z)
    }

    pub const fn xy(self) -> IVec2 {
        IVec2::new(self.x, self.y)
    }

    /// Центр клетки в координатах грида.
    pub fn center(self) -> Vec2 {
        Vec2::new(self.x as f32 + 0.5, self.y as f32 + 0.5)
    }

    /// Соседняя клетка того же этажа.
    pub const fn offset(self, by: IVec2) -> Self {
        Self::new(self.x + by.x, self.y + by.y, self.z)
    }

    /// Чанк клетки.
    pub const fn chunk(self) -> ChunkPos {
        ChunkPos {
            x: self.x.div_euclid(CHUNK_SIZE),
            y: self.y.div_euclid(CHUNK_SIZE),
            z: self.z,
        }
    }

    /// Номер клетки внутри чанка: строки по `y`, в строке — по `x`.
    pub const fn index_in_chunk(self) -> usize {
        (self.x.rem_euclid(CHUNK_SIZE) + self.y.rem_euclid(CHUNK_SIZE) * CHUNK_SIZE) as usize
    }
}

/// Чанк грида: квадрат [`CHUNK_SIZE`]×[`CHUNK_SIZE`] тайлов на этаже `z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i16,
}

impl ChunkPos {
    /// Клетка чанка по её номеру внутри него.
    pub const fn tile(self, index: usize) -> TileCoord {
        TileCoord::new(
            self.x * CHUNK_SIZE + index as i32 % CHUNK_SIZE,
            self.y * CHUNK_SIZE + index as i32 / CHUNK_SIZE,
            self.z,
        )
    }
}

/// Положение относительно сущности-родителя: карты, грида или контейнера.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityCoordinates {
    pub parent: Entity,
    pub position: Vec2,
    /// Этаж относительно родителя.
    pub z: i16,
}

impl EntityCoordinates {
    pub const fn new(parent: Entity, position: Vec2) -> Self {
        Self {
            parent,
            position,
            z: 0,
        }
    }

    pub const fn on_level(parent: Entity, position: Vec2, z: i16) -> Self {
        Self {
            parent,
            position,
            z,
        }
    }
}

/// Положение на карте в мировых координатах.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapCoordinates {
    pub map: MapId,
    pub position: Vec2,
    /// Этаж карты.
    pub z: i16,
}

impl MapCoordinates {
    pub const fn new(map: MapId, position: Vec2) -> Self {
        Self {
            map,
            position,
            z: 0,
        }
    }

    pub const fn on_level(map: MapId, position: Vec2, z: i16) -> Self {
        Self { map, position, z }
    }

    /// Точки на одной карте и одном этаже не дальше `range` друг от друга.
    pub fn in_range(&self, other: &MapCoordinates, range: f32) -> bool {
        self.map == other.map
            && self.z == other.z
            && self.position.distance_squared(other.position) <= range * range
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_chunk_math_handles_negative_coordinates() {
        let tile = TileCoord::containing(Vec2::new(-0.5, 16.2), 2);
        assert_eq!(tile, TileCoord::new(-1, 16, 2));
        assert_eq!(tile.chunk(), ChunkPos { x: -1, y: 1, z: 2 });
        assert_eq!(tile.index_in_chunk(), 15);
        assert_eq!(tile.chunk().tile(tile.index_in_chunk()), tile);
        assert_eq!(tile.center(), Vec2::new(-0.5, 16.5));
        for (x, y) in [(0, 0), (15, 15), (-16, -1), (31, -17)] {
            let tile = TileCoord::new(x, y, 0);
            assert_eq!(tile.chunk().tile(tile.index_in_chunk()), tile);
        }
    }

    #[test]
    fn range_respects_map_and_level() {
        let a = MapCoordinates::new(MapId(1), Vec2::ZERO);
        assert!(a.in_range(&MapCoordinates::new(MapId(1), Vec2::new(3.0, 4.0)), 5.0));
        assert!(!a.in_range(&MapCoordinates::new(MapId(2), Vec2::ZERO), 5.0));
        assert!(!a.in_range(&MapCoordinates::on_level(MapId(1), Vec2::ZERO, 1), 5.0));
    }
}
