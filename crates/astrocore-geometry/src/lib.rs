use std::collections::BTreeMap;

use astrocore_core::PrototypeId;
use astrocore_math::{Aabb, ChunkCoord, EdgeDir, Facing, GridCell, GridEdge, glam::Vec3};
use astrocore_world::{FloorDef, FloorId, Placement, StructureElement, StructureKind, World};

/// Индексированная геометрия. Все позиции — в мировых координатах (Y-up, метры),
/// треугольники обходятся против часовой стрелки при взгляде снаружи.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn bounds(&self) -> Option<Aabb> {
        let mut it = self.positions.iter().map(|p| Vec3::from_array(*p));
        let first = it.next()?;
        let (min, max) = it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
        Some(Aabb { min, max })
    }

    /// Четырёхугольник с заданной наружной нормалью. Если обход вершин не согласован
    /// с нормалью, порядок разворачивается — треугольники всегда смотрят наружу.
    fn push_quad(&mut self, corners: [Vec3; 4], normal: Vec3) {
        let mut corners = corners;
        let face = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
        let face = if face.length_squared() < 1e-12 {
            (corners[2] - corners[0]).cross(corners[3] - corners[0])
        } else {
            face
        };
        if face.dot(normal) < 0.0 {
            corners.reverse();
        }
        let base = self.positions.len() as u32;
        for (c, uv) in corners
            .iter()
            .zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
        {
            self.positions.push(c.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Параллелепипед из шести граней с наружными нормалями.
    fn push_box(&mut self, min: Vec3, max: Vec3) {
        let (x0, y0, z0) = (min.x, min.y, min.z);
        let (x1, y1, z1) = (max.x, max.y, max.z);
        // +X
        self.push_quad(
            [
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y1, z0),
                Vec3::new(x1, y1, z1),
            ],
            Vec3::X,
        );
        // −X
        self.push_quad(
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y1, z1),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::NEG_X,
        );
        // +Y
        self.push_quad(
            [
                Vec3::new(x0, y1, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x1, y1, z0),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::Y,
        );
        // −Y
        self.push_quad(
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y0, z1),
                Vec3::new(x0, y0, z1),
            ],
            Vec3::NEG_Y,
        );
        // +Z
        self.push_quad(
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x0, y1, z1),
            ],
            Vec3::Z,
        );
        // −Z
        self.push_quad(
            [
                Vec3::new(x1, y0, z0),
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y1, z0),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::NEG_Z,
        );
    }
}

/// Меши одного чанка на одном этаже, сгруппированные по материалу (батчинг по материалам).
pub mod glb;

pub type ChunkMeshes = BTreeMap<PrototypeId, MeshData>;

/// Габариты элемента в мировых координатах.
pub fn element_bounds(world: &World, floor: &FloorDef, e: &StructureElement) -> Option<Aabb> {
    let step = world.config().grid_step_m;
    let t = e.thickness;
    match (e.kind, e.placement) {
        (StructureKind::Wall, Placement::Edge(edge)) => {
            Some(wall_box_joined(world, floor, edge, t))
        }
        (StructureKind::Floor, Placement::Cell(c)) => {
            let min = c.min_corner(step, floor.base_y);
            Some(Aabb::new(min, min + Vec3::new(step, t, step)))
        }
        (StructureKind::Ceiling, Placement::Cell(c)) => {
            let min = c.min_corner(step, floor.top_y() - t);
            Some(Aabb::new(min, min + Vec3::new(step, t, step)))
        }
        (
            StructureKind::Stair { to } | StructureKind::Ramp { to },
            Placement::Run { start, dir, length },
        ) => {
            let top = world.floor(to)?;
            let (min, max) = run_footprint(start, dir, length, step);
            Some(Aabb::new(
                Vec3::new(min.x, floor.base_y, min.z),
                Vec3::new(max.x, top.base_y, max.z),
            ))
        }
        (StructureKind::Freeform, Placement::Free(f)) => {
            let (min, max) = f.aabb(step);
            Some(Aabb::new(Vec3::from_array(min), Vec3::from_array(max)))
        }
        // Проём геометрии не порождает.
        _ => None,
    }
}

/// Прямоугольник марша в плоскости X/Z: `(min, max)` по осям X/Z (Y = 0).
fn run_footprint(start: GridCell, dir: Facing, length: u8, step: f32) -> (Vec3, Vec3) {
    let end = start.step(dir, i32::from(length) - 1);
    let (a, b) = (start.min_corner(step, 0.0), end.min_corner(step, 0.0));
    (a.min(b), a.max(b) + Vec3::new(step, 0.0, step))
}

/// Коллизионные коробки элемента («cooking»). Стена, перекрытие — одна коробка;
/// лестница — по коробке на ступень; пандус — тонкие срезы (аппроксимация склона
/// ступенями высотой не более [`RAMP_SLICE_M`]).
pub fn collision_boxes(world: &World, floor: &FloorDef, e: &StructureElement) -> Vec<Aabb> {
    let step = world.config().grid_step_m;
    match (e.kind, e.placement) {
        (StructureKind::Stair { to }, Placement::Run { start, dir, length }) => world
            .floor(to)
            .map(|top| {
                run_slices(
                    step,
                    floor.base_y,
                    top.base_y,
                    start,
                    dir,
                    length,
                    stair_step_count(top.base_y - floor.base_y),
                )
            })
            .unwrap_or_default(),
        (StructureKind::Ramp { to }, Placement::Run { start, dir, length }) => world
            .floor(to)
            .map(|top| {
                let n = (((top.base_y - floor.base_y) / RAMP_SLICE_M).ceil() as usize).max(1);
                run_slices(step, floor.base_y, top.base_y, start, dir, length, n)
            })
            .unwrap_or_default(),
        // Окно непроходимо; дверь — динамический объект (см. `opening_bounds`).
        (StructureKind::Opening(astrocore_world::OpeningKind::Window), Placement::Edge(edge)) => {
            vec![wall_box(edge, step, e.thickness, floor)]
        }
        (StructureKind::Opening(astrocore_world::OpeningKind::Door), _) => Vec::new(),
        _ => element_bounds(world, floor, e).into_iter().collect(),
    }
}

/// Габариты проёма (двери) как тонкой коробки по ребру — для динамической коллизии
/// закрытой двери.
pub fn opening_bounds(world: &World, e: &StructureElement) -> Option<Aabb> {
    let floor = world.floor(e.floor)?;
    match (e.kind, e.placement) {
        (StructureKind::Opening(_), Placement::Edge(edge)) => Some(wall_box(
            edge,
            world.config().grid_step_m,
            e.thickness,
            floor,
        )),
        _ => None,
    }
}

/// Максимальная высота ступени аппроксимации пандуса, метры.
pub const RAMP_SLICE_M: f32 = 0.1;

/// Разбиение марша на `n` последовательных коробок, высота растёт вдоль направления.
fn run_slices(
    step: f32,
    base_y: f32,
    top_y: f32,
    start: GridCell,
    dir: Facing,
    length: u8,
    n: usize,
) -> Vec<Aabb> {
    let (min, max) = run_footprint(start, dir, length, step);
    let (rise, run_len) = (top_y - base_y, f32::from(length) * step);
    (0..n)
        .map(|i| {
            let (t0, t1) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
            let (mut lo, mut hi) = (
                Vec3::new(min.x, base_y, min.z),
                Vec3::new(max.x, base_y + rise * t1, max.z),
            );
            match dir {
                Facing::East => {
                    lo.x = min.x + run_len * t0;
                    hi.x = min.x + run_len * t1;
                }
                Facing::West => {
                    lo.x = max.x - run_len * t1;
                    hi.x = max.x - run_len * t0;
                }
                Facing::South => {
                    lo.z = min.z + run_len * t0;
                    hi.z = min.z + run_len * t1;
                }
                Facing::North => {
                    lo.z = max.z - run_len * t1;
                    hi.z = max.z - run_len * t0;
                }
            }
            Aabb::new(lo, hi)
        })
        .collect()
}

/// Высота одной ступени по умолчанию, метры.
pub const STAIR_RISER_M: f32 = 0.2;

/// Число ступеней марша: не больше `STAIR_RISER_M` на ступень.
pub fn stair_step_count(rise_m: f32) -> usize {
    ((rise_m / STAIR_RISER_M).ceil() as usize).max(1)
}

/// Есть ли стена на ребре этажа.
fn has_wall(world: &World, floor: FloorId, edge: GridEdge) -> bool {
    world
        .edge_element(floor, edge)
        .is_some_and(|e| matches!(e.kind, StructureKind::Wall))
}

/// Стена с «сваркой» углов: на конце, где к вершине примыкает перпендикулярная стена,
/// коробка удлиняется на половину толщины — угол и Т-образный стык заполнены без щелей.
fn wall_box_joined(world: &World, floor: &FloorDef, edge: GridEdge, thickness: f32) -> Aabb {
    let step = world.config().grid_step_m;
    let mut b = wall_box(edge, step, thickness, floor);
    let half = thickness * 0.5;
    let (x, z) = (edge.x, edge.z);
    // Перпендикулярные рёбра, примыкающие к начальной и к конечной вершине.
    let (start_perp, end_perp) = match edge.dir {
        EdgeDir::AlongX => (
            [GridEdge::along_z(x, z), GridEdge::along_z(x, z - 1)],
            [GridEdge::along_z(x + 1, z), GridEdge::along_z(x + 1, z - 1)],
        ),
        EdgeDir::AlongZ => (
            [GridEdge::along_x(x, z), GridEdge::along_x(x - 1, z)],
            [GridEdge::along_x(x, z + 1), GridEdge::along_x(x - 1, z + 1)],
        ),
    };
    let joined = |edges: [GridEdge; 2]| edges.into_iter().any(|e| has_wall(world, floor.id, e));
    let axis = match edge.dir {
        EdgeDir::AlongX => 0,
        EdgeDir::AlongZ => 2,
    };
    if joined(start_perp) {
        b.min[axis] -= half;
    }
    if joined(end_perp) {
        b.max[axis] += half;
    }
    b
}

fn wall_box(edge: GridEdge, step: f32, thickness: f32, floor: &FloorDef) -> Aabb {
    let (a, b) = edge.endpoints(step, floor.base_y);
    let half = thickness * 0.5;
    let (min, max) = match edge.dir {
        EdgeDir::AlongX => (
            a - Vec3::new(0.0, 0.0, half),
            b + Vec3::new(0.0, floor.height, half),
        ),
        EdgeDir::AlongZ => (
            a - Vec3::new(half, 0.0, 0.0),
            b + Vec3::new(half, floor.height, 0.0),
        ),
    };
    Aabb::new(min, max)
}

/// Собрать меши чанка на этаже. Порядок детерминирован (по `ObjectId`).
pub fn build_chunk_meshes(world: &World, chunk: ChunkCoord, floor_id: FloorId) -> ChunkMeshes {
    build_chunk_meshes_filtered(world, chunk, floor_id, &|_| true)
}

/// То же, но в меши попадают только элементы, для которых `keep` вернул `true`
/// (режимы просмотра редактора: срез этажей, скрытые потолки).
pub fn build_chunk_meshes_filtered(
    world: &World,
    chunk: ChunkCoord,
    floor_id: FloorId,
    keep: &dyn Fn(&StructureElement) -> bool,
) -> ChunkMeshes {
    let mut out = ChunkMeshes::new();
    let Some(floor) = world.floor(floor_id) else {
        return out;
    };
    for e in world.elements_in_chunk(chunk, floor_id) {
        // Элемент на границе чанков принадлежит обоим, но геометрию эмитит только
        // чанк, в котором лежит «первая» ячейка элемента, чтобы грани не дублировались.
        if owner_chunk(world, e) != chunk || !keep(e) {
            continue;
        }
        let step = world.config().grid_step_m;
        match (e.kind, e.placement) {
            (StructureKind::Stair { to }, Placement::Run { start, dir, length }) => {
                if let Some(top) = world.floor(to) {
                    let mesh = out.entry(e.material.clone()).or_default();
                    push_stair(mesh, step, floor.base_y, top.base_y, start, dir, length);
                }
            }
            (StructureKind::Ramp { to }, Placement::Run { start, dir, length }) => {
                if let Some(top) = world.floor(to) {
                    let mesh = out.entry(e.material.clone()).or_default();
                    push_ramp(mesh, step, floor.base_y, top.base_y, start, dir, length);
                }
            }
            // Свободные модели рисует клиент по идентификатору модели; в структурные меши они не входят.
            (StructureKind::Freeform, _) => {}
            _ => {
                if let Some(b) = element_bounds(world, floor, e) {
                    out.entry(e.material.clone())
                        .or_default()
                        .push_box(b.min, b.max);
                }
            }
        }
    }
    out
}

/// Ступени: каждая — параллелепипед от земли до своей высоты (сплошной марш).
fn push_stair(
    mesh: &mut MeshData,
    step: f32,
    base_y: f32,
    top_y: f32,
    start: GridCell,
    dir: Facing,
    length: u8,
) {
    let n = stair_step_count(top_y - base_y);
    for b in run_slices(step, base_y, top_y, start, dir, length, n) {
        mesh.push_box(b.min, b.max);
    }
}

/// Пандус: клин, поднимающийся вдоль направления.
fn push_ramp(
    mesh: &mut MeshData,
    step: f32,
    base_y: f32,
    top_y: f32,
    start: GridCell,
    dir: Facing,
    length: u8,
) {
    let (min, max) = run_footprint(start, dir, length, step);
    // Четыре нижние угла и высоты верхних углов: низ — у входа, верх — у выхода.
    let corner = |x: f32, z: f32, along: f32| {
        (
            Vec3::new(x, base_y, z),
            Vec3::new(x, base_y + (top_y - base_y) * along, z),
        )
    };
    let along = |x: f32, z: f32| -> f32 {
        match dir {
            Facing::East => (x - min.x) / (max.x - min.x),
            Facing::West => (max.x - x) / (max.x - min.x),
            Facing::South => (z - min.z) / (max.z - min.z),
            Facing::North => (max.z - z) / (max.z - min.z),
        }
    };
    let pts = [
        (min.x, min.z),
        (max.x, min.z),
        (max.x, max.z),
        (min.x, max.z),
    ]
    .map(|(x, z)| corner(x, z, along(x, z)));
    let (b, t) = (pts.map(|p| p.0), pts.map(|p| p.1));
    mesh.push_quad([b[0], b[1], b[2], b[3]].map(|p| p), Vec3::NEG_Y);
    // Поверхность (нормаль считается по вершинам, обход против часовой стрелки сверху).
    let n = (t[3] - t[0]).cross(t[1] - t[0]).normalize_or_zero();
    mesh.push_quad([t[3], t[2], t[1], t[0]], if n.y < 0.0 { -n } else { n });
    // Боковые и торцевые грани — четырёхугольники между низом и верхом.
    for (i, j) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        let mut normal = (b[j] - b[i]).cross(Vec3::Y).normalize_or_zero();
        let mid = (b[i] + b[j]) * 0.5;
        let center = (min + max) * 0.5;
        if (mid - center).dot(normal) < 0.0 {
            normal = -normal; // наружу от центра основания
        }
        mesh.push_quad([b[i], b[j], t[j], t[i]], normal);
    }
}

fn owner_chunk(world: &World, e: &StructureElement) -> ChunkCoord {
    let cfg = world.config();
    let cell: GridCell = e
        .placement
        .touched_cells()
        .into_iter()
        .max()
        .expect("у элемента всегда есть ячейка");
    cell.chunk(cfg.grid_step_m, cfg.chunk_size_m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrocore_core::ObjectId;
    use astrocore_world::{WorldCommand, WorldConfig};

    fn world() -> World {
        let mut w = World::new("t", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 2.0,
            height: 3.5,
        }))
        .unwrap();
        w
    }

    fn put(w: &mut World, id: u128, kind: StructureKind, placement: Placement, mat: &str) {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement,
            floor: FloorId(0),
            material: PrototypeId::new(mat).unwrap(),
            thickness: 0.2,
        }))
        .unwrap();
    }

    #[test]
    fn wall_spans_floor_height_along_y() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(2, 3)),
            "t:stone",
        );
        let meshes = build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0));
        let m = &meshes[&PrototypeId::new("t:stone").unwrap()];
        let b = m.bounds().unwrap();
        assert_eq!(
            (b.min.y, b.max.y),
            (2.0, 5.5),
            "высота = высота этажа, ось Y"
        );
        assert!((b.min.x - 2.0).abs() < 1e-6 && (b.max.x - 3.0).abs() < 1e-6);
        assert!((b.min.z - 2.9).abs() < 1e-5 && (b.max.z - 3.1).abs() < 1e-5);
        assert_eq!(m.triangle_count(), 12);
        assert_eq!(m.positions.len(), 24);
    }

    #[test]
    fn along_z_wall_is_thin_in_x() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(4, 0)),
            "t:stone",
        );
        let m = &build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0))
            [&PrototypeId::new("t:stone").unwrap()];
        let s = m.bounds().unwrap().size();
        assert!((s.x - 0.2).abs() < 1e-5 && (s.z - 1.0).abs() < 1e-6);
    }

    #[test]
    fn meshes_are_batched_by_material() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(0, 0)),
            "t:stone",
        );
        put(
            &mut w,
            2,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(1, 0)),
            "t:stone",
        );
        put(
            &mut w,
            3,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(2, 0)),
            "t:wood",
        );
        let meshes = build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0));
        assert_eq!(meshes.len(), 2);
        assert_eq!(
            meshes[&PrototypeId::new("t:stone").unwrap()].triangle_count(),
            24
        );
        assert_eq!(
            meshes[&PrototypeId::new("t:wood").unwrap()].triangle_count(),
            12
        );
    }

    #[test]
    fn openings_produce_no_geometry() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Opening(astrocore_world::OpeningKind::Door),
            Placement::Edge(GridEdge::along_x(0, 0)),
            "t:door",
        );
        assert!(build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0)).is_empty());
    }

    #[test]
    fn boundary_element_is_emitted_exactly_once() {
        let mut w = world();
        // Ребро между чанками (0,0) и (1,0).
        put(
            &mut w,
            1,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(32, 0)),
            "t:stone",
        );
        let a = build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0));
        let b = build_chunk_meshes(&w, ChunkCoord::new(1, 0), FloorId(0));
        let total: usize = a
            .values()
            .chain(b.values())
            .map(MeshData::triangle_count)
            .sum();
        assert_eq!(total, 12);
    }

    #[test]
    fn slabs_sit_on_floor_and_below_ceiling() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Floor,
            Placement::Cell(GridCell::new(0, 0)),
            "t:stone",
        );
        put(
            &mut w,
            2,
            StructureKind::Ceiling,
            Placement::Cell(GridCell::new(0, 0)),
            "t:wood",
        );
        let meshes = build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0));
        let floor = meshes[&PrototypeId::new("t:stone").unwrap()]
            .bounds()
            .unwrap();
        let ceiling = meshes[&PrototypeId::new("t:wood").unwrap()]
            .bounds()
            .unwrap();
        assert_eq!(floor.min.y, 2.0);
        assert!((ceiling.max.y - 5.5).abs() < 1e-6);
    }

    #[test]
    fn unknown_floor_gives_empty_result_and_output_is_deterministic() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(0, 0)),
            "t:stone",
        );
        assert!(build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(9)).is_empty());
        assert_eq!(
            build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0)),
            build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0))
        );
    }

    #[test]
    fn normals_are_unit_and_outward() {
        let mut m = MeshData::default();
        m.push_box(Vec3::ZERO, Vec3::ONE);
        let center = Vec3::splat(0.5);
        for (p, n) in m.positions.iter().zip(&m.normals) {
            let (p, n) = (Vec3::from_array(*p), Vec3::from_array(*n));
            assert!((n.length() - 1.0).abs() < 1e-6);
            assert!((p - center).dot(n) > 0.0, "нормаль направлена наружу");
        }
        // Обход треугольников согласован с нормалью.
        for tri in m.indices.chunks(3) {
            let v: Vec<Vec3> = tri
                .iter()
                .map(|&i| Vec3::from_array(m.positions[i as usize]))
                .collect();
            let face = (v[1] - v[0]).cross(v[2] - v[0]).normalize();
            let n = Vec3::from_array(m.normals[tri[0] as usize]);
            assert!(
                face.dot(n) > 0.99,
                "порядок обхода против часовой стрелки снаружи"
            );
        }
    }

    mod stairs {
        use super::*;
        use astrocore_world::{FloorDef, WorldCommand};

        fn two_floors() -> World {
            let mut w = world();
            w.apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(1),
                name: "up".into(),
                base_y: 6.0,
                height: 3.0,
            }))
            .unwrap();
            w
        }

        fn run(w: &mut World, id: u128, kind: StructureKind, dir: Facing, len: u8) {
            w.apply(WorldCommand::PlaceElement(StructureElement {
                id: ObjectId::from_u128(id),
                kind,
                placement: Placement::Run {
                    start: GridCell::new(2, 3),
                    dir,
                    length: len,
                },
                floor: FloorId(0),
                material: PrototypeId::new("t:stone").unwrap(),
                thickness: 0.2,
            }))
            .unwrap();
        }

        #[test]
        fn stair_has_one_box_per_step_and_reaches_the_upper_floor() {
            let mut w = two_floors();
            run(
                &mut w,
                1,
                StructureKind::Stair { to: FloorId(1) },
                Facing::East,
                5,
            );
            let m = &build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0))
                [&PrototypeId::new("t:stone").unwrap()];
            // Подъём 4 м (2.0 → 6.0): ceil(4 / 0.2) = 20 ступеней по 12 треугольников.
            assert_eq!(stair_step_count(4.0), 20);
            assert_eq!(m.triangle_count(), 20 * 12);
            let b = m.bounds().unwrap();
            assert!(
                (b.min.y - 2.0).abs() < 1e-5 && (b.max.y - 6.0).abs() < 1e-5,
                "{b:?}"
            );
            // Марш на 5 ячеек вдоль +X начинается в x=2 и занимает ширину одной ячейки по Z.
            assert!((b.min.x - 2.0).abs() < 1e-5 && (b.max.x - 7.0).abs() < 1e-5);
            assert!((b.min.z - 3.0).abs() < 1e-5 && (b.max.z - 4.0).abs() < 1e-5);
        }

        #[test]
        fn stair_height_grows_along_the_direction_for_every_facing() {
            for dir in [Facing::East, Facing::West, Facing::South, Facing::North] {
                let mut w = two_floors();
                run(&mut w, 1, StructureKind::Stair { to: FloorId(1) }, dir, 4);
                let m = &build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0))
                    [&PrototypeId::new("t:stone").unwrap()];
                let along = |p: &[f32; 3]| Vec3::from_array(*p).dot(dir.as_vec3());
                let (lo, hi) = m
                    .positions
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                        (lo.min(along(p)), hi.max(along(p)))
                    });
                let high_end = m
                    .positions
                    .iter()
                    .filter(|p| (along(p) - hi).abs() < 1e-5)
                    .map(|p| p[1])
                    .fold(f32::MIN, f32::max);
                let low_end = m
                    .positions
                    .iter()
                    .filter(|p| (along(p) - lo).abs() < 1e-5)
                    .map(|p| p[1])
                    .fold(f32::MIN, f32::max);
                assert!(
                    high_end > low_end + 3.0,
                    "{dir:?}: верх ступеней у выхода выше, чем у входа ({low_end} → {high_end})"
                );
            }
        }

        #[test]
        fn ramp_is_a_convex_wedge_with_outward_normals() {
            let mut w = two_floors();
            run(
                &mut w,
                1,
                StructureKind::Ramp { to: FloorId(1) },
                Facing::South,
                4,
            );
            let m = &build_chunk_meshes(&w, ChunkCoord::new(0, 0), FloorId(0))
                [&PrototypeId::new("t:stone").unwrap()];
            assert_eq!(
                m.triangle_count(),
                12,
                "низ, поверхность и четыре боковых грани"
            );
            let b = m.bounds().unwrap();
            assert!((b.max.y - 6.0).abs() < 1e-5 && (b.min.y - 2.0).abs() < 1e-5);
            let center = b.center();
            for tri in m.indices.chunks(3) {
                let v: Vec<Vec3> = tri
                    .iter()
                    .map(|&i| Vec3::from_array(m.positions[i as usize]))
                    .collect();
                let face = (v[1] - v[0]).cross(v[2] - v[0]);
                if face.length_squared() < 1e-12 {
                    continue; // вырожденный треугольник боковой грани клина
                }
                let outward = (v[0] - center).dot(face.normalize());
                assert!(outward > -1e-4, "грань смотрит внутрь: {outward}");
            }
        }

        #[test]
        fn stair_bounds_match_footprint() {
            let mut w = two_floors();
            run(
                &mut w,
                1,
                StructureKind::Stair { to: FloorId(1) },
                Facing::North,
                3,
            );
            let e = w.element(ObjectId::from_u128(1)).unwrap().clone();
            let b = element_bounds(&w, w.floor(FloorId(0)).unwrap(), &e).unwrap();
            // Марш на север из (2,3): ячейки z = 3, 2, 1 → z ∈ [1, 4).
            assert!(
                (b.min.z - 1.0).abs() < 1e-5 && (b.max.z - 4.0).abs() < 1e-5,
                "{b:?}"
            );
        }
    }

    mod collision {
        use super::*;
        use astrocore_world::{FloorDef, WorldCommand};

        fn two_floors() -> World {
            let mut w = world();
            w.apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(1),
                name: "up".into(),
                base_y: 6.0,
                height: 3.0,
            }))
            .unwrap();
            w
        }

        fn put_run(w: &mut World, kind: StructureKind) -> StructureElement {
            let e = StructureElement {
                id: ObjectId::from_u128(1),
                kind,
                placement: Placement::Run {
                    start: GridCell::new(0, 0),
                    dir: Facing::East,
                    length: 4,
                },
                floor: FloorId(0),
                material: PrototypeId::new("t:m").unwrap(),
                thickness: 0.2,
            };
            w.apply(WorldCommand::PlaceElement(e.clone())).unwrap();
            e
        }

        #[test]
        fn walls_and_slabs_have_one_box() {
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:m",
            );
            let e = w.element(ObjectId::from_u128(1)).unwrap().clone();
            let boxes = collision_boxes(&w, w.floor(FloorId(0)).unwrap(), &e);
            assert_eq!(boxes.len(), 1);
        }

        #[test]
        fn stair_boxes_form_a_walkable_staircase() {
            let mut w = two_floors();
            let e = put_run(&mut w, StructureKind::Stair { to: FloorId(1) });
            let boxes = collision_boxes(&w, w.floor(FloorId(0)).unwrap(), &e);
            assert_eq!(boxes.len(), stair_step_count(4.0));
            // Высота ступеней строго растёт вдоль +X; соседние ступени вплотную по X.
            for pair in boxes.windows(2) {
                assert!(pair[1].max.y > pair[0].max.y);
                assert!((pair[0].max.x - pair[1].min.x).abs() < 1e-5);
                assert!(
                    pair[1].max.y - pair[0].max.y <= STAIR_RISER_M + 1e-4,
                    "подступенок не выше нормы"
                );
            }
            assert!((boxes.last().unwrap().max.y - 6.0).abs() < 1e-5);
        }

        #[test]
        fn ramp_is_sliced_finer_than_a_stair() {
            let mut w = two_floors();
            let e = put_run(&mut w, StructureKind::Ramp { to: FloorId(1) });
            let boxes = collision_boxes(&w, w.floor(FloorId(0)).unwrap(), &e);
            assert_eq!(boxes.len(), 40);
            assert!(boxes[1].max.y - boxes[0].max.y <= RAMP_SLICE_M + 1e-4);
        }

        #[test]
        fn openings_have_no_collision() {
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Opening(astrocore_world::OpeningKind::Door),
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:d",
            );
            let e = w.element(ObjectId::from_u128(1)).unwrap().clone();
            assert!(collision_boxes(&w, w.floor(FloorId(0)).unwrap(), &e).is_empty());
        }
    }

    mod joins {
        use super::*;
        use astrocore_world::WorldCommand;

        fn bounds_of(w: &World, id: u128) -> Aabb {
            let e = w.element(ObjectId::from_u128(id)).unwrap().clone();
            element_bounds(w, w.floor(FloorId(0)).unwrap(), &e).unwrap()
        }

        #[test]
        fn corner_walls_extend_to_fill_the_notch() {
            let mut w = world();
            // Угол: стена вдоль X от (0,0) и стена вдоль Z от (0,0).
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:m",
            );
            put(
                &mut w,
                2,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(0, 0)),
                "t:m",
            );
            let (a, b) = (bounds_of(&w, 1), bounds_of(&w, 2));
            // Обе стены толщиной 0.2: каждая заходит за вершину на 0.1.
            assert!(
                (a.min.x + 0.1).abs() < 1e-5 && (a.max.x - 1.0).abs() < 1e-5,
                "{a:?}"
            );
            assert!(
                (b.min.z + 0.1).abs() < 1e-5 && (b.max.z - 1.0).abs() < 1e-5,
                "{b:?}"
            );
        }

        #[test]
        fn isolated_and_collinear_walls_are_not_extended() {
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:m",
            );
            put(
                &mut w,
                2,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(1, 0)),
                "t:m",
            );
            assert!(
                (bounds_of(&w, 1).max.x - 1.0).abs() < 1e-5,
                "продолжение по прямой стыкуется без захода"
            );
            assert!((bounds_of(&w, 2).min.x - 1.0).abs() < 1e-5);
        }

        #[test]
        fn t_junction_extends_only_the_end_that_touches() {
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:m",
            );
            put(
                &mut w,
                2,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(1, 0)),
                "t:m",
            );
            put(
                &mut w,
                3,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(1, 0)),
                "t:m",
            ); // отходит от стыка
            let stem = bounds_of(&w, 3);
            assert!(
                (stem.min.z + 0.1).abs() < 1e-5,
                "ножка заходит в основную стену: {stem:?}"
            );
            assert!((stem.max.z - 1.0).abs() < 1e-5);
        }

        #[test]
        fn openings_do_not_join_and_removing_a_neighbour_restores_the_box() {
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(0, 0)),
                "t:m",
            );
            put(
                &mut w,
                2,
                StructureKind::Opening(astrocore_world::OpeningKind::Door),
                Placement::Edge(GridEdge::along_z(0, 0)),
                "t:d",
            );
            assert!(
                (bounds_of(&w, 1).min.x).abs() < 1e-5,
                "рядом только дверь — захода нет"
            );
            put(
                &mut w,
                3,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(0, -1)),
                "t:m",
            );
            assert!((bounds_of(&w, 1).min.x + 0.1).abs() < 1e-5);
            w.apply(WorldCommand::RemoveElement {
                id: ObjectId::from_u128(3),
            })
            .unwrap();
            assert!((bounds_of(&w, 1).min.x).abs() < 1e-5);
        }

        #[test]
        fn joined_walls_share_dirty_neighbourhood() {
            // Изменение соседа меняет габариты стены, поэтому оба элемента обязаны попадать в один dirty-чанк.
            let mut w = world();
            put(
                &mut w,
                1,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(31, 0)),
                "t:m",
            );
            w.take_dirty();
            put(
                &mut w,
                2,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(32, 0)),
                "t:m",
            );
            let chunks: Vec<_> = w.take_dirty().into_iter().map(|d| d.chunk).collect();
            assert!(
                chunks.contains(&ChunkCoord::new(0, 0)) && chunks.contains(&ChunkCoord::new(1, 0))
            );
        }
    }
}
