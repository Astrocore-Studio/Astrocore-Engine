//! Координатная конвенция Astrocore (ADR-0003, AC-011).
//!
//! * единицы — метры;
//! * система правосторонняя, **Y — вверх** (как в Bevy);
//! * горизонтальная плоскость — X/Z, «вперёд» для камеры и персонажа — −Z;
//! * углы — радианы, положительный поворот вокруг +Y — против часовой стрелки
//!   при взгляде сверху вниз (правило правой руки).
//!
//! Во всём ядре (домен, рендер, физика, навигация) используется одна и та же
//! система осей, поэтому конвертеров осей здесь нет. Преобразование допустимо
//! только в импортёрах внешних данных — см. [`import`].

use glam::Vec3;
use serde::{Deserialize, Serialize};

pub use glam;

/// Вектор «вверх».
pub const UP: Vec3 = Vec3::Y;
/// Направление «вперёд» по умолчанию (как у камеры Bevy).
pub const FORWARD: Vec3 = Vec3::NEG_Z;
/// Направление «вправо» по умолчанию.
pub const RIGHT: Vec3 = Vec3::X;

/// Стартовая гипотеза размера чанка по горизонтали, метры (AC-013: настраиваемая).
pub const DEFAULT_CHUNK_SIZE_M: f32 = 32.0;
/// Стартовая гипотеза шага структурной сетки, метры (AC-013: настраиваемая).
pub const DEFAULT_GRID_STEP_M: f32 = 1.0;

/// Горизонтальная координата чанка `(x, z)`. Высота в индекс чанка не входит.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChunkCoord {
    pub x: i32,
    pub z: i32,
}

impl ChunkCoord {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// Чанк, содержащий мировую точку. Корректно для отрицательных координат
    /// (используется `floor`, а не усечение к нулю).
    pub fn from_world(pos: Vec3, chunk_size_m: f32) -> Self {
        Self {
            x: (pos.x / chunk_size_m).floor() as i32,
            z: (pos.z / chunk_size_m).floor() as i32,
        }
    }

    /// Мировая позиция минимального угла чанка (по высоте — 0).
    pub fn min_corner(self, chunk_size_m: f32) -> Vec3 {
        Vec3::new(
            self.x as f32 * chunk_size_m,
            0.0,
            self.z as f32 * chunk_size_m,
        )
    }
}

/// Целочисленная ячейка структурной сетки в горизонтальной плоскости X/Z.
/// Ячейка `(x, z)` покрывает `[x*step, (x+1)*step) × [z*step, (z+1)*step)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GridCell {
    pub x: i32,
    pub z: i32,
}

impl GridCell {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// Ячейка, содержащая мировую точку.
    pub fn from_world(pos: Vec3, step_m: f32) -> Self {
        Self {
            x: (pos.x / step_m).floor() as i32,
            z: (pos.z / step_m).floor() as i32,
        }
    }

    /// Мировая позиция минимального угла ячейки при заданной высоте `y`.
    pub fn min_corner(self, step_m: f32, y: f32) -> Vec3 {
        Vec3::new(self.x as f32 * step_m, y, self.z as f32 * step_m)
    }

    /// Чанк, которому принадлежит ячейка.
    pub fn chunk(self, step_m: f32, chunk_size_m: f32) -> ChunkCoord {
        // Ячейка целиком лежит внутри одного чанка, если размер чанка кратен шагу.
        // Используем минимальный угол ячейки: floor делит корректно для любых знаков.
        ChunkCoord::from_world(self.min_corner(step_m, 0.0), chunk_size_m)
    }

    pub const fn offset(self, dx: i32, dz: i32) -> Self {
        Self {
            x: self.x + dx,
            z: self.z + dz,
        }
    }
}

/// Ребро сетки: отрезок между двумя соседними вершинами сетки. Стены и проёмы
/// лежат на рёбрах.
///
/// Вершина `(x, z)` — это точка `(x*step, z*step)`.
/// * `AlongX` — ребро от `(x, z)` до `(x+1, z)`; разделяет ячейки `(x, z-1)` и `(x, z)`.
/// * `AlongZ` — ребро от `(x, z)` до `(x, z+1)`; разделяет ячейки `(x-1, z)` и `(x, z)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GridEdge {
    pub x: i32,
    pub z: i32,
    pub dir: EdgeDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeDir {
    AlongX,
    AlongZ,
}

impl GridEdge {
    pub const fn along_x(x: i32, z: i32) -> Self {
        Self {
            x,
            z,
            dir: EdgeDir::AlongX,
        }
    }

    pub const fn along_z(x: i32, z: i32) -> Self {
        Self {
            x,
            z,
            dir: EdgeDir::AlongZ,
        }
    }

    /// Две ячейки, которые разделяет ребро.
    pub const fn adjacent_cells(self) -> [GridCell; 2] {
        match self.dir {
            EdgeDir::AlongX => [
                GridCell::new(self.x, self.z - 1),
                GridCell::new(self.x, self.z),
            ],
            EdgeDir::AlongZ => [
                GridCell::new(self.x - 1, self.z),
                GridCell::new(self.x, self.z),
            ],
        }
    }

    /// Ребро между двумя соседними по стороне ячейками (если они действительно соседи).
    pub fn between(a: GridCell, b: GridCell) -> Option<Self> {
        match (b.x - a.x, b.z - a.z) {
            (1, 0) => Some(Self::along_z(b.x, a.z)),
            (-1, 0) => Some(Self::along_z(a.x, a.z)),
            (0, 1) => Some(Self::along_x(a.x, b.z)),
            (0, -1) => Some(Self::along_x(a.x, a.z)),
            _ => None,
        }
    }

    /// Начальная и конечная вершина ребра в мировых координатах при высоте `y`.
    pub fn endpoints(self, step_m: f32, y: f32) -> (Vec3, Vec3) {
        let a = Vec3::new(self.x as f32 * step_m, y, self.z as f32 * step_m);
        let b = match self.dir {
            EdgeDir::AlongX => a + Vec3::new(step_m, 0.0, 0.0),
            EdgeDir::AlongZ => a + Vec3::new(0.0, 0.0, step_m),
        };
        (a, b)
    }
}

/// Осеориентированный ограничивающий параллелепипед.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self {
            min: min.min(max),
            max: min.max(max),
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

/// Импортёры внешних данных. Это **единственное** место, где допускается
/// преобразование осей (ADR-0003).
pub mod import {
    use glam::Vec3;

    /// Z-up (Blender, CAD) → Y-up Astrocore, правосторонняя система сохраняется:
    /// `(x, y, z)_zup → (x, z, -y)_yup`.
    pub fn from_z_up(p: Vec3) -> Vec3 {
        Vec3::new(p.x, p.z, -p.y)
    }

    /// Обратное преобразование: Y-up Astrocore → Z-up.
    pub fn to_z_up(p: Vec3) -> Vec3 {
        Vec3::new(p.x, -p.z, p.y)
    }

    /// 2D-карта (x, y) → горизонтальная плоскость X/Z на заданной высоте.
    /// Ось 2D-карты Y (вниз по экрану, как в SS14) отображается в +Z.
    pub fn from_map_2d(x: f32, y: f32, height: f32) -> Vec3 {
        Vec3::new(x, height, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convention_is_right_handed_y_up() {
        // right × up = back (+Z) для правосторонней системы с «вперёд» = −Z.
        assert_eq!(RIGHT.cross(UP), -FORWARD);
        assert_eq!(FORWARD.cross(UP), RIGHT);
    }

    #[test]
    fn chunk_from_world_handles_negative_coordinates() {
        let s = DEFAULT_CHUNK_SIZE_M;
        assert_eq!(
            ChunkCoord::from_world(Vec3::new(0.0, 5.0, 0.0), s),
            ChunkCoord::new(0, 0)
        );
        assert_eq!(
            ChunkCoord::from_world(Vec3::new(31.99, 0.0, 31.99), s),
            ChunkCoord::new(0, 0)
        );
        assert_eq!(
            ChunkCoord::from_world(Vec3::new(32.0, 0.0, 0.0), s),
            ChunkCoord::new(1, 0)
        );
        assert_eq!(
            ChunkCoord::from_world(Vec3::new(-0.01, 0.0, -0.01), s),
            ChunkCoord::new(-1, -1)
        );
        assert_eq!(
            ChunkCoord::from_world(Vec3::new(-32.0, 0.0, -32.01), s),
            ChunkCoord::new(-1, -2)
        );
    }

    #[test]
    fn height_does_not_change_chunk() {
        let s = DEFAULT_CHUNK_SIZE_M;
        let low = ChunkCoord::from_world(Vec3::new(5.0, -8.0, 5.0), s);
        let high = ChunkCoord::from_world(Vec3::new(5.0, 300.0, 5.0), s);
        assert_eq!(low, high);
    }

    #[test]
    fn grid_cell_chunk_crosses_boundary_correctly() {
        let (g, c) = (DEFAULT_GRID_STEP_M, DEFAULT_CHUNK_SIZE_M);
        assert_eq!(GridCell::new(31, 31).chunk(g, c), ChunkCoord::new(0, 0));
        assert_eq!(GridCell::new(32, 0).chunk(g, c), ChunkCoord::new(1, 0));
        assert_eq!(GridCell::new(-1, -1).chunk(g, c), ChunkCoord::new(-1, -1));
        assert_eq!(GridCell::new(-32, -33).chunk(g, c), ChunkCoord::new(-1, -2));
    }

    #[test]
    fn edge_adjacent_cells_and_between_are_consistent() {
        let a = GridCell::new(3, 4);
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let b = a.offset(dx, dz);
            let e = GridEdge::between(a, b).expect("соседи по стороне");
            let cells = e.adjacent_cells();
            assert!(cells.contains(&a) && cells.contains(&b), "{e:?}");
            assert_eq!(GridEdge::between(b, a), Some(e), "симметрия");
        }
        assert_eq!(GridEdge::between(a, a.offset(1, 1)), None);
        assert_eq!(GridEdge::between(a, a), None);
    }

    #[test]
    fn edge_endpoints_follow_axes() {
        let (a, b) = GridEdge::along_x(2, 3).endpoints(2.0, 1.0);
        assert_eq!((a, b), (Vec3::new(4.0, 1.0, 6.0), Vec3::new(6.0, 1.0, 6.0)));
        let (a, b) = GridEdge::along_z(2, 3).endpoints(2.0, 1.0);
        assert_eq!((a, b), (Vec3::new(4.0, 1.0, 6.0), Vec3::new(4.0, 1.0, 8.0)));
    }

    #[test]
    fn z_up_import_roundtrips_and_keeps_handedness() {
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(import::to_z_up(import::from_z_up(p)), p);
        // «Вверх» в Z-up (0,0,1) обязан стать «вверх» Astrocore.
        assert_eq!(import::from_z_up(Vec3::Z), UP);
        // Правосторонность: X × Y_zup = Z_zup и после импорта X × (−Z) = Y.
        let x = import::from_z_up(Vec3::X);
        let y = import::from_z_up(Vec3::Y);
        let z = import::from_z_up(Vec3::Z);
        assert_eq!(x.cross(y), z);
    }

    #[test]
    fn map_2d_import_maps_to_horizontal_plane() {
        assert_eq!(import::from_map_2d(5.0, 7.0, 3.0), Vec3::new(5.0, 3.0, 7.0));
    }

    #[test]
    fn aabb_normalizes_and_unions() {
        let a = Aabb::new(Vec3::new(1.0, 1.0, 1.0), Vec3::ZERO);
        assert_eq!(a.min, Vec3::ZERO);
        let b = Aabb::new(Vec3::splat(-1.0), Vec3::splat(0.5));
        let u = a.union(&b);
        assert_eq!((u.min, u.max), (Vec3::splat(-1.0), Vec3::ONE));
        assert_eq!(a.size(), Vec3::ONE);
    }
}
