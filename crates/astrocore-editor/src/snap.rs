use astrocore_core::ObjectId;
use astrocore_math::glam::Vec3;
use astrocore_math::{GridCell, GridEdge};
use astrocore_physics::{BoxWorld, SpatialQuery};

/// Ближайшая вершина сетки.
pub fn snap_vertex(p: Vec3, step: f32) -> (i32, i32) {
    ((p.x / step).round() as i32, (p.z / step).round() as i32)
}

/// Ячейка под точкой.
pub fn snap_cell(p: Vec3, step: f32) -> GridCell {
    GridCell::from_world(p, step)
}

/// Ближайшее к точке ребро сетки (по горизонтальному расстоянию).
/// При равенстве приоритет: север, юг, запад, восток.
pub fn nearest_edge(p: Vec3, step: f32) -> GridEdge {
    let cell = snap_cell(p, step);
    let u = p.x / step - cell.x as f32; // 0..1 внутри ячейки по X
    let v = p.z / step - cell.z as f32; // 0..1 по Z
    let candidates = [
        (v, GridEdge::along_x(cell.x, cell.z)),           // север
        (1.0 - v, GridEdge::along_x(cell.x, cell.z + 1)), // юг
        (u, GridEdge::along_z(cell.x, cell.z)),           // запад
        (1.0 - u, GridEdge::along_z(cell.x + 1, cell.z)), // восток
    ];
    candidates
        .into_iter()
        .fold((f32::INFINITY, candidates[0].1), |best, c| {
            if c.0 < best.0 { c } else { best }
        })
        .1
}

/// Точка и нормаль поверхности под лучом курсора (для размещения мелких объектов).
pub fn snap_to_surface(
    bw: &BoxWorld,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(Vec3, Vec3)> {
    bw.raycast(origin, dir, max_distance)
        .map(|h| (h.point, h.normal))
}

/// Объект под лучом курсора (для выделения щелчком).
pub fn pick_element(bw: &BoxWorld, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<ObjectId> {
    bw.raycast(origin, dir, max_distance).map(|h| h.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_and_cell_snapping_handle_negative_coordinates() {
        assert_eq!(snap_vertex(Vec3::new(2.4, 0.0, 2.6), 1.0), (2, 3));
        assert_eq!(snap_vertex(Vec3::new(-0.4, 0.0, -0.6), 1.0), (0, -1));
        assert_eq!(
            snap_vertex(Vec3::new(5.0, 0.0, 5.0), 2.0),
            (3, 3),
            "шаг сетки 2 м"
        );
        assert_eq!(
            snap_cell(Vec3::new(-0.1, 9.0, -0.1), 1.0),
            GridCell::new(-1, -1)
        );
    }

    #[test]
    fn nearest_edge_picks_the_closest_side() {
        let e = |x: f32, z: f32| nearest_edge(Vec3::new(x, 0.0, z), 1.0);
        assert_eq!(e(2.5, 3.05), GridEdge::along_x(2, 3), "север ячейки (2,3)");
        assert_eq!(e(2.5, 3.95), GridEdge::along_x(2, 4), "юг");
        assert_eq!(e(2.05, 3.5), GridEdge::along_z(2, 3), "запад");
        assert_eq!(e(2.95, 3.5), GridEdge::along_z(3, 3), "восток");
        assert_eq!(e(-0.95, -0.5), GridEdge::along_z(-1, -1));
    }

    #[test]
    fn nearest_edge_is_adjacent_to_the_cell_under_the_cursor() {
        for (x, z) in [(0.3, 0.7), (-4.2, 8.8), (12.5, -3.5), (7.01, 7.99)] {
            let p = Vec3::new(x, 0.0, z);
            assert!(
                nearest_edge(p, 1.0)
                    .adjacent_cells()
                    .contains(&snap_cell(p, 1.0)),
                "{x},{z}"
            );
        }
    }
}
