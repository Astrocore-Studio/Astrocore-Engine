//! Генерация мешей структурной геометрии (раздел 9.1 дизайн-документа).
//!
//! Крейт не знает про GPU и Bevy: результат — обычные массивы вершин/индексов
//! ([`MeshData`]), сгруппированные по материалу. Клиентский рендер превращает их
//! в Bevy-меши, а сервер может использовать для коллизий.
//!
//! Известные ограничения текущей версии (см. `docs/PROGRESS.md`): внутренние
//! грани между соседними стенами не удаляются, углы не скругляются, проёмы не
//! вырезаются из геометрии (проём — это отсутствие стены на ребре).

use std::collections::BTreeMap;

use astrocore_core::PrototypeId;
use astrocore_math::{Aabb, ChunkCoord, EdgeDir, Facing, GridCell, GridEdge, glam::Vec3};
use astrocore_world::{FloorDef, FloorId, Placement, StructureElement, StructureKind, World};

/// Индексированная геометрия. Все позиции — в мировых координатах (Y-up, метры),
/// треугольники обходятся против часовой стрелки при взгляде снаружи.
#[derive(Debug, Clone, Default, PartialEq)]
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
pub type ChunkMeshes = BTreeMap<PrototypeId, MeshData>;

/// Габариты элемента в мировых координатах.
pub fn element_bounds(world: &World, floor: &FloorDef, e: &StructureElement) -> Option<Aabb> {
    let step = world.config().grid_step_m;
    let t = e.thickness;
    match (e.kind, e.placement) {
        (StructureKind::Wall, Placement::Edge(edge)) => Some(wall_box(edge, step, t, floor)),
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

/// Высота одной ступени по умолчанию, метры.
pub const STAIR_RISER_M: f32 = 0.2;

/// Число ступеней марша: не больше `STAIR_RISER_M` на ступень.
pub fn stair_step_count(rise_m: f32) -> usize {
    ((rise_m / STAIR_RISER_M).ceil() as usize).max(1)
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
    let mut out = ChunkMeshes::new();
    let Some(floor) = world.floor(floor_id) else {
        return out;
    };
    for e in world.elements_in_chunk(chunk, floor_id) {
        // Элемент на границе чанков принадлежит обоим, но геометрию эмитит только
        // чанк, в котором лежит «первая» ячейка элемента, чтобы грани не дублировались.
        if owner_chunk(world, e) != chunk {
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
    let (min, max) = run_footprint(start, dir, length, step);
    let n = stair_step_count(top_y - base_y);
    let (rise, run_len) = (top_y - base_y, f32::from(length) * step);
    for i in 0..n {
        let (t0, t1) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
        let height = base_y + rise * t1;
        let (mut lo, mut hi) = (
            Vec3::new(min.x, base_y, min.z),
            Vec3::new(max.x, height, max.z),
        );
        // Сужаем бокс до доли марша вдоль направления подъёма.
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
        mesh.push_box(lo, hi);
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
}
