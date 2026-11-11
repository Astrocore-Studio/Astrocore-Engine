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

/// Одно из четырёх горизонтальных направлений (вдоль осей сетки).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Facing {
    /// +X
    East,
    /// −X
    West,
    /// +Z
    South,
    /// −Z (направление «вперёд» по умолчанию)
    North,
}

impl Facing {
    /// Смещение в ячейках.
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Facing::East => (1, 0),
            Facing::West => (-1, 0),
            Facing::South => (0, 1),
            Facing::North => (0, -1),
        }
    }

    pub const fn as_vec3(self) -> Vec3 {
        match self {
            Facing::East => Vec3::X,
            Facing::West => Vec3::NEG_X,
            Facing::South => Vec3::Z,
            Facing::North => Vec3::NEG_Z,
        }
    }
}

impl GridCell {
    /// Ячейка на `n` шагов в направлении `dir`.
    pub const fn step(self, dir: Facing, n: i32) -> Self {
        let (dx, dz) = dir.delta();
        Self {
            x: self.x + dx * n,
            z: self.z + dz * n,
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

/// Преобразование сетки: отражение по X, поворот на четверти оборота, перенос.
///
/// Порядок: сначала отражение (`x → −x`), затем поворот, затем перенос. Поворот на
/// одну четверть — «по часовой стрелке» при взгляде сверху, когда +X вправо, а +Z
/// вниз: вершина `(x, z) → (−z, x)`, `East → South → West → North`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct GridTransform {
    pub mirror_x: bool,
    /// Число четвертей оборота (берётся по модулю 4).
    pub quarter_turns: u8,
    pub dx: i32,
    pub dz: i32,
}

impl GridTransform {
    pub const IDENTITY: GridTransform = GridTransform {
        mirror_x: false,
        quarter_turns: 0,
        dx: 0,
        dz: 0,
    };

    pub const fn translate(dx: i32, dz: i32) -> Self {
        Self {
            mirror_x: false,
            quarter_turns: 0,
            dx,
            dz,
        }
    }

    pub const fn rotate(quarter_turns: u8) -> Self {
        Self {
            mirror_x: false,
            quarter_turns: quarter_turns % 4,
            dx: 0,
            dz: 0,
        }
    }

    /// Преобразование вершины сетки.
    pub fn vertex(self, (x, z): (i32, i32)) -> (i32, i32) {
        let (mut x, mut z) = (if self.mirror_x { -x } else { x }, z);
        for _ in 0..(self.quarter_turns % 4) {
            (x, z) = (-z, x);
        }
        (x + self.dx, z + self.dz)
    }

    /// Ячейка = квадрат из четырёх вершин; результат — ячейка с минимальным углом.
    pub fn cell(self, c: GridCell) -> GridCell {
        let corners = [
            (c.x, c.z),
            (c.x + 1, c.z),
            (c.x, c.z + 1),
            (c.x + 1, c.z + 1),
        ]
        .map(|v| self.vertex(v));
        GridCell::new(
            corners.iter().map(|v| v.0).min().unwrap_or(0),
            corners.iter().map(|v| v.1).min().unwrap_or(0),
        )
    }

    pub fn edge(self, e: GridEdge) -> GridEdge {
        let (a, b) = match e.dir {
            EdgeDir::AlongX => ((e.x, e.z), (e.x + 1, e.z)),
            EdgeDir::AlongZ => ((e.x, e.z), (e.x, e.z + 1)),
        };
        let (a, b) = (self.vertex(a), self.vertex(b));
        let min = (a.0.min(b.0), a.1.min(b.1));
        if a.0 == b.0 {
            GridEdge::along_z(min.0, min.1)
        } else {
            GridEdge::along_x(min.0, min.1)
        }
    }

    pub fn facing(self, f: Facing) -> Facing {
        let mut f = if self.mirror_x {
            match f {
                Facing::East => Facing::West,
                Facing::West => Facing::East,
                other => other,
            }
        } else {
            f
        };
        for _ in 0..(self.quarter_turns % 4) {
            f = match f {
                Facing::East => Facing::South,
                Facing::South => Facing::West,
                Facing::West => Facing::North,
                Facing::North => Facing::East,
            };
        }
        f
    }

    /// Прямой марш: начальная ячейка и направление преобразуются, длина сохраняется.
    pub fn run(self, start: GridCell, dir: Facing) -> (GridCell, Facing) {
        (self.cell(start), self.facing(dir))
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
    fn facing_deltas_match_axes() {
        assert_eq!(Facing::North.as_vec3(), FORWARD);
        assert_eq!(Facing::East.as_vec3(), RIGHT);
        assert_eq!(
            GridCell::new(1, 1).step(Facing::South, 3),
            GridCell::new(1, 4)
        );
        assert_eq!(
            GridCell::new(1, 1).step(Facing::West, 2),
            GridCell::new(-1, 1)
        );
    }

    #[test]
    fn transform_rotation_is_consistent_for_vertices_cells_edges_and_facing() {
        let r = GridTransform::rotate(1);
        assert_eq!(r.vertex((3, 2)), (-2, 3));
        assert_eq!(r.facing(Facing::East), Facing::South);
        // Четыре поворота — тождество, для любых объектов.
        let r4 = GridTransform::rotate(4);
        let (c, e) = (GridCell::new(3, -2), GridEdge::along_z(5, 1));
        assert_eq!((r4.cell(c), r4.edge(e)), (c, e));
        let mut cur = (c, e, Facing::North);
        for _ in 0..4 {
            cur = (r.cell(cur.0), r.edge(cur.1), r.facing(cur.2));
        }
        assert_eq!(cur, (c, e, Facing::North));
    }

    #[test]
    fn transformed_edge_stays_between_transformed_adjacent_cells() {
        // Главный инвариант: ребро между двумя ячейками остаётся ребром между их образами.
        for quarter_turns in 0..4 {
            for mirror_x in [false, true] {
                let t = GridTransform {
                    mirror_x,
                    quarter_turns,
                    dx: 7,
                    dz: -3,
                };
                for edge in [
                    GridEdge::along_x(2, 5),
                    GridEdge::along_z(-4, 1),
                    GridEdge::along_x(0, 0),
                ] {
                    let [a, b] = edge.adjacent_cells();
                    let expected = GridEdge::between(t.cell(a), t.cell(b));
                    assert_eq!(Some(t.edge(edge)), expected, "{t:?} {edge:?}");
                }
            }
        }
    }

    #[test]
    fn transformed_run_cells_are_the_images_of_run_cells() {
        let t = GridTransform {
            mirror_x: true,
            quarter_turns: 3,
            dx: 2,
            dz: 9,
        };
        let (start, dir, len) = (GridCell::new(1, 2), Facing::East, 4);
        let (s2, d2) = t.run(start, dir);
        for i in 0..len {
            assert_eq!(s2.step(d2, i), t.cell(start.step(dir, i)), "i={i}");
        }
    }

    #[test]
    fn mirror_swaps_east_west_and_translate_moves() {
        let m = GridTransform {
            mirror_x: true,
            ..GridTransform::IDENTITY
        };
        assert_eq!(m.facing(Facing::East), Facing::West);
        assert_eq!(m.facing(Facing::North), Facing::North);
        assert_eq!(m.cell(GridCell::new(0, 0)), GridCell::new(-1, 0));
        assert_eq!(
            GridTransform::translate(3, 4).cell(GridCell::new(1, 1)),
            GridCell::new(4, 5)
        );
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
