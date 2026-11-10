use astrocore_core::ObjectId;
use astrocore_geometry::build_chunk_meshes;
use astrocore_math::GridCell;
use astrocore_navigation::{PathOptions, find_path};
use astrocore_spatial::Topology;
use astrocore_vertical_slice::{BREACH_EDGE, MIN_X, Slice};
use astrocore_world::FloorId;

fn main() {
    let mut s = Slice::build().expect("сценарий строится");
    println!(
        "мир: {} элементов, ревизия {}",
        s.world.element_count(),
        s.world.revision().0
    );

    for chunk in s.world.populated_chunks(FloorId(0)) {
        let tris: usize = build_chunk_meshes(&s.world, chunk, FloorId(0))
            .values()
            .map(|m| m.triangle_count())
            .sum();
        println!(
            "  чанк ({}, {}): {tris} треугольников на 1-м этаже",
            chunk.x, chunk.z
        );
    }

    let topo = Topology::build(&s.world, FloorId(0)).expect("топология");
    println!(
        "1-й этаж: комнат {}, порталов {}",
        topo.rooms.len(),
        topo.portals.len()
    );

    let (outside, inside) = (GridCell::new(MIN_X + 2, -2), GridCell::new(MIN_X + 2, 2));
    let closed = PathOptions {
        closed_doors: [s.door]
            .into_iter()
            .collect::<std::collections::HashSet<ObjectId>>(),
        ..Default::default()
    };
    println!(
        "путь при закрытой двери: {:?}",
        find_path(&s.world, FloorId(0), outside, inside, &closed).map(|p| p.len())
    );
    s.breach().expect("пролом");
    println!(
        "после пролома {BREACH_EDGE:?}: путь при закрытой двери: {:?}",
        find_path(&s.world, FloorId(0), outside, inside, &closed).map(|p| p.len())
    );
}
