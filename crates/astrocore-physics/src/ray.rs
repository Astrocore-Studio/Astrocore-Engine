//! Лучи: что пересекает отрезок на карте. Образец — `IntersectRayWithPredicate`
//! (`SharedPhysicsSystem.Queries`) Robust @ 1afd692 и `RayCast` фигур Box2D.
//!
//! Луч проверяется в космосе карты и в каждом гриде на ней. Учитываются только твёрдые
//! фикстуры, слой которых пересекается с маской луча. Фигура, внутри которой начинается
//! луч, его не останавливает — как в Box2D.

use astrocore_map::{Map, MapCoordinates, MapWorldExt, TransformWorldExt};
use astrocore_math::{Box2, Vec2};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use crate::body::{CollisionGroups, Fixtures, Physics};
use crate::shapes::WorldShape;
use crate::world::{PhysicsWorldExt, placement};

/// Попадание луча.
#[derive(Debug, Clone, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    pub fixture: String,
    /// Расстояние от начала луча.
    pub distance: f32,
    /// Точка попадания в координатах карты.
    pub point: Vec2,
}

/// Луч из точки карты.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: MapCoordinates,
    /// Направление; длина не важна.
    pub direction: Vec2,
    pub max_distance: f32,
    pub mask: CollisionGroups,
}

/// Доля пути от `origin` по `direction`, на которой луч входит в фигуру; `None` — не
/// входит в пределах `max` или начинается внутри.
fn enter(shape: &WorldShape, origin: Vec2, direction: Vec2, max: f32) -> Option<f32> {
    match shape {
        WorldShape::Circle { center, radius } => {
            let to_origin = origin - *center;
            let b = to_origin.dot(direction);
            let c = to_origin.length_squared() - radius * radius;
            if c <= 0.0 {
                return None;
            }
            let sigma = b * b - c;
            if sigma < 0.0 {
                return None;
            }
            let t = -b - sigma.sqrt();
            (t >= 0.0 && t <= max).then_some(t)
        }
        WorldShape::Polygon(vertices) => {
            let (mut lower, mut upper) = (0.0f32, max);
            let mut entered = false;
            for (index, a) in vertices.iter().enumerate() {
                let b = vertices[(index + 1) % vertices.len()];
                let edge = b - *a;
                // Вершины против часовой стрелки: внешняя нормаль — поворот ребра по часовой.
                let normal = Vec2::new(edge.y, -edge.x);
                let numerator = normal.dot(*a - origin);
                let denominator = normal.dot(direction);
                if denominator == 0.0 {
                    if numerator < 0.0 {
                        return None;
                    }
                } else if denominator < 0.0 && numerator < lower * denominator {
                    lower = numerator / denominator;
                    entered = true;
                } else if denominator > 0.0 && numerator < upper * denominator {
                    upper = numerator / denominator;
                }
                if upper < lower {
                    return None;
                }
            }
            entered.then_some(lower)
        }
    }
}

/// Лучи на `World`.
pub trait RayWorldExt {
    /// Попадания луча по возрастанию расстояния. `ignore` пропускает сущности; `first_only`
    /// возвращает только ближайшее попадание.
    fn intersect_ray(
        &self,
        ray: Ray,
        ignore: &dyn Fn(Entity) -> bool,
        first_only: bool,
    ) -> Vec<RayHit>;

    /// Расстояние до первой преграды на отрезке или длина отрезка, если преград нет.
    fn unobstructed_distance(&self, ray: Ray, ignore: &dyn Fn(Entity) -> bool) -> f32;
}

impl RayWorldExt for World {
    fn intersect_ray(
        &self,
        ray: Ray,
        ignore: &dyn Fn(Entity) -> bool,
        first_only: bool,
    ) -> Vec<RayHit> {
        let length = ray.direction.length();
        let Some(map) = self.map_entity(ray.origin.map) else {
            return Vec::new();
        };
        if length == 0.0 || ray.max_distance <= 0.0 {
            return Vec::new();
        }
        let direction = ray.direction / length;
        let end = ray.origin.position + direction * ray.max_distance;
        let roots = std::iter::once(map).chain(
            self.get::<Map>(map)
                .map(|map| map.grids().to_vec())
                .unwrap_or_default(),
        );
        let mut hits = Vec::new();
        for root in roots {
            // Луч переводится в оси корня: длины при этом не меняются.
            let (start_local, end_local, level) = if root == map {
                (ray.origin.position, end, ray.origin.z)
            } else {
                let Some(placement) = self.world_transform(root) else {
                    continue;
                };
                (
                    placement.to_local(ray.origin.position),
                    placement.to_local(end),
                    ray.origin.z - placement.level,
                )
            };
            let local_direction = (end_local - start_local) / ray.max_distance;
            let bounds = Box2::from_corners(start_local.min(end_local), start_local.max(end_local));
            for entity in self.entities_intersecting(root, level, bounds) {
                if ignore(entity) {
                    continue;
                }
                let (Some(fixtures), Some(body), Some((_, _, pose))) = (
                    self.get::<Fixtures>(entity),
                    self.get::<Physics>(entity),
                    placement(self, entity),
                ) else {
                    continue;
                };
                if !body.can_collide {
                    continue;
                }
                let nearest = fixtures
                    .fixtures
                    .iter()
                    .filter(|(_, fixture)| fixture.hard && fixture.layer.intersects(ray.mask))
                    .filter_map(|(name, fixture)| {
                        let t = enter(
                            &fixture.shape.at(&pose),
                            start_local,
                            local_direction,
                            ray.max_distance,
                        )?;
                        Some((t, name))
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((distance, name)) = nearest {
                    hits.push(RayHit {
                        entity,
                        fixture: name.clone(),
                        distance,
                        point: ray.origin.position + direction * distance,
                    });
                }
            }
        }
        hits.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        if first_only {
            hits.truncate(1);
        }
        hits
    }

    fn unobstructed_distance(&self, ray: Ray, ignore: &dyn Fn(Entity) -> bool) -> f32 {
        self.intersect_ray(ray, ignore, true)
            .first()
            .map_or(ray.max_distance, |hit| hit.distance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rays_enter_circles_and_polygons_from_outside_only() {
        let circle = WorldShape::Circle {
            center: Vec2::new(5.0, 0.0),
            radius: 1.0,
        };
        let right = Vec2::X;
        assert_eq!(enter(&circle, Vec2::ZERO, right, 10.0), Some(4.0));
        assert_eq!(enter(&circle, Vec2::ZERO, right, 3.0), None);
        assert_eq!(enter(&circle, Vec2::ZERO, Vec2::Y, 10.0), None);
        assert_eq!(enter(&circle, Vec2::new(5.0, 0.0), right, 10.0), None);

        let square = WorldShape::Polygon(
            [
                Vec2::new(2.0, -1.0),
                Vec2::new(4.0, -1.0),
                Vec2::new(4.0, 1.0),
                Vec2::new(2.0, 1.0),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(enter(&square, Vec2::ZERO, right, 10.0), Some(2.0));
        assert_eq!(enter(&square, Vec2::new(6.0, 0.0), -right, 10.0), Some(2.0));
        assert_eq!(enter(&square, Vec2::ZERO, right, 1.5), None);
        assert_eq!(enter(&square, Vec2::new(0.0, 2.0), right, 10.0), None);
        assert_eq!(enter(&square, Vec2::new(3.0, 0.0), right, 10.0), None);
        let diagonal = Vec2::new(1.0, 1.0).normalize();
        let t = enter(&square, Vec2::new(0.0, -2.0), diagonal, 10.0).unwrap();
        assert!((t - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-5, "{t}");
    }
}

/// Расстояние от точки до отрезка.
fn point_segment(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let edge = b - a;
    let t = if edge.length_squared() == 0.0 {
        0.0
    } else {
        ((point - a).dot(edge) / edge.length_squared()).clamp(0.0, 1.0)
    };
    point.distance(a + edge * t)
}

/// Точка внутри выпуклого многоугольника против часовой стрелки.
fn inside(point: Vec2, vertices: &[Vec2]) -> bool {
    vertices.iter().enumerate().all(|(index, a)| {
        let b = vertices[(index + 1) % vertices.len()];
        let edge = b - *a;
        edge.x * (point.y - a.y) - edge.y * (point.x - a.x) >= 0.0
    })
}

/// Наименьшее расстояние между фигурами; 0 — фигуры пересекаются (`TryGetNearest` Robust).
pub fn shape_distance(a: &WorldShape, b: &WorldShape) -> f32 {
    let edges = |vertices: &[Vec2]| -> Vec<(Vec2, Vec2)> {
        (0..vertices.len())
            .map(|i| (vertices[i], vertices[(i + 1) % vertices.len()]))
            .collect()
    };
    let to_polygon = |point: Vec2, vertices: &[Vec2]| -> f32 {
        if inside(point, vertices) {
            return 0.0;
        }
        edges(vertices)
            .into_iter()
            .map(|(p, q)| point_segment(point, p, q))
            .fold(f32::INFINITY, f32::min)
    };
    match (a, b) {
        (
            WorldShape::Circle {
                center: ca,
                radius: ra,
            },
            WorldShape::Circle {
                center: cb,
                radius: rb,
            },
        ) => (ca.distance(*cb) - ra - rb).max(0.0),
        (WorldShape::Circle { center, radius }, WorldShape::Polygon(vertices))
        | (WorldShape::Polygon(vertices), WorldShape::Circle { center, radius }) => {
            (to_polygon(*center, vertices) - radius).max(0.0)
        }
        (WorldShape::Polygon(pa), WorldShape::Polygon(pb)) => {
            // Выпуклые многоугольники пересекаются, если вершина одного внутри другого или
            // их рёбра пересекаются; иначе ближе всего вершина одного к ребру другого.
            if pa.iter().any(|v| inside(*v, pb)) || pb.iter().any(|v| inside(*v, pa)) {
                return 0.0;
            }
            let cross =
                |o: Vec2, p: Vec2, q: Vec2| (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
            for (p1, p2) in edges(pa) {
                for (q1, q2) in edges(pb) {
                    let d1 = cross(q1, q2, p1);
                    let d2 = cross(q1, q2, p2);
                    let d3 = cross(p1, p2, q1);
                    let d4 = cross(p1, p2, q2);
                    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
                        return 0.0;
                    }
                }
            }
            let one_way = |from: &[Vec2], to: &[Vec2]| {
                from.iter()
                    .map(|v| {
                        edges(to)
                            .into_iter()
                            .map(|(p, q)| point_segment(*v, p, q))
                            .fold(f32::INFINITY, f32::min)
                    })
                    .fold(f32::INFINITY, f32::min)
            };
            one_way(pa, pb).min(one_way(pb, pa))
        }
    }
}

/// Расстояния между телами на `World`.
pub trait DistanceWorldExt {
    /// Наименьшее расстояние между фикстурами двух сущностей на одной карте и этаже;
    /// `None` — у одной нет фикстур или они на разных картах или этажах.
    fn nearest_distance(&self, a: Entity, b: Entity) -> Option<f32>;
}

impl DistanceWorldExt for World {
    fn nearest_distance(&self, a: Entity, b: Entity) -> Option<f32> {
        let shapes = |entity: Entity| -> Option<(MapCoordinates, Vec<WorldShape>)> {
            let fixtures = self.get::<Fixtures>(entity)?;
            if fixtures.fixtures.is_empty() {
                return None;
            }
            let at = self.map_coordinates(entity)?;
            let placement = self.world_transform(entity)?;
            let pose = crate::shapes::Pose {
                position: placement.position,
                rotation: placement.rotation,
            };
            Some((
                at,
                fixtures
                    .fixtures
                    .values()
                    .map(|f| f.shape.at(&pose))
                    .collect(),
            ))
        };
        let (at_a, shapes_a) = shapes(a)?;
        let (at_b, shapes_b) = shapes(b)?;
        if at_a.map != at_b.map || at_a.z != at_b.z {
            return None;
        }
        let mut nearest = f32::INFINITY;
        for shape_a in &shapes_a {
            for shape_b in &shapes_b {
                nearest = nearest.min(shape_distance(shape_a, shape_b));
            }
        }
        Some(nearest)
    }
}

#[cfg(test)]
mod distance_tests {
    use super::*;

    fn square(x: f32, y: f32, half: f32) -> WorldShape {
        WorldShape::Polygon(
            [
                Vec2::new(x - half, y - half),
                Vec2::new(x + half, y - half),
                Vec2::new(x + half, y + half),
                Vec2::new(x - half, y + half),
            ]
            .into_iter()
            .collect(),
        )
    }

    #[test]
    fn distances_between_shapes() {
        let circle = |x: f32, y: f32, r: f32| WorldShape::Circle {
            center: Vec2::new(x, y),
            radius: r,
        };
        assert!(
            (shape_distance(&circle(0.0, 0.0, 0.5), &circle(3.0, 0.0, 0.5)) - 2.0).abs() < 1e-5
        );
        assert_eq!(
            shape_distance(&circle(0.0, 0.0, 1.0), &circle(1.0, 0.0, 1.0)),
            0.0
        );
        // Круг у грани и у угла квадрата.
        assert!(
            (shape_distance(&circle(0.0, 0.0, 0.35), &square(2.0, 0.0, 0.5)) - 1.15).abs() < 1e-5
        );
        let corner = shape_distance(&circle(0.0, 0.0, 0.5), &square(2.0, 2.0, 0.5));
        assert!(
            (corner - (1.5f32 * std::f32::consts::SQRT_2 - 0.5)).abs() < 1e-5,
            "{corner}"
        );
        assert_eq!(
            shape_distance(&circle(2.0, 0.0, 0.1), &square(2.0, 0.0, 0.5)),
            0.0
        );
        // Квадраты: рядом, вплотную, внахлёст, крестом без вершин внутри.
        assert!(
            (shape_distance(&square(0.0, 0.0, 0.5), &square(3.0, 0.0, 0.5)) - 2.0).abs() < 1e-5
        );
        assert_eq!(
            shape_distance(&square(0.0, 0.0, 0.5), &square(1.0, 0.0, 0.5)),
            0.0
        );
        assert_eq!(
            shape_distance(&square(0.0, 0.0, 0.5), &square(0.5, 0.5, 0.5)),
            0.0
        );
        let wide = WorldShape::Polygon(
            [
                Vec2::new(-2.0, -0.1),
                Vec2::new(2.0, -0.1),
                Vec2::new(2.0, 0.1),
                Vec2::new(-2.0, 0.1),
            ]
            .into_iter()
            .collect(),
        );
        let tall = WorldShape::Polygon(
            [
                Vec2::new(-0.1, -2.0),
                Vec2::new(0.1, -2.0),
                Vec2::new(0.1, 2.0),
                Vec2::new(-0.1, 2.0),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(shape_distance(&wide, &tall), 0.0);
    }
}
