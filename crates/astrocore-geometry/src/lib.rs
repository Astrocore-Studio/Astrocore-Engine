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
use astrocore_math::{Aabb, ChunkCoord, EdgeDir, GridCell, GridEdge, glam::Vec3};
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

    fn push_quad(&mut self, corners: [Vec3; 4], normal: Vec3) {
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
        // Проём геометрии не порождает.
        _ => None,
    }
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
        if let Some(b) = element_bounds(world, floor, e) {
            out.entry(e.material.clone())
                .or_default()
                .push_box(b.min, b.max);
        }
    }
    out
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
}
