//! Формы фикстур и поиск пересечений. Образец — `Robust.Shared/Physics/Collision` @ 1afd692
//! (порт Box2D): круг и выпуклый многоугольник; прямоугольник — многоугольник из четырёх
//! вершин.

use astrocore_math::{Angle, Box2, Vec2};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Форма фикстуры в координатах сущности. Имена вариантов — как в данных SS14
/// (`!type:PhysShapeAabb`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    PhysShapeAabb {
        #[serde(default = "unit_box")]
        bounds: Box2,
    },
    PhysShapeCircle {
        #[serde(default = "default_radius")]
        radius: f32,
        #[serde(default)]
        position: Vec2,
    },
    PolygonShape {
        vertices: Vec<Vec2>,
    },
}

fn unit_box() -> Box2 {
    Box2::new(-0.5, -0.5, 0.5, 0.5)
}

fn default_radius() -> f32 {
    0.5
}

impl Default for Shape {
    fn default() -> Self {
        Shape::PhysShapeAabb { bounds: unit_box() }
    }
}

/// Положение тела в координатах его грида или карты.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: Vec2,
    pub rotation: Angle,
}

impl Pose {
    pub fn point(&self, local: Vec2) -> Vec2 {
        self.rotation.rotate(local) + self.position
    }
}

/// Форма в координатах грида или карты.
#[derive(Debug, Clone, PartialEq)]
pub enum WorldShape {
    Circle {
        center: Vec2,
        radius: f32,
    },
    /// Выпуклый многоугольник, вершины против часовой стрелки.
    Polygon(SmallVec<[Vec2; 8]>),
}

impl Shape {
    /// Площадь формы: по ней и плотности считается масса.
    pub fn area(&self) -> f32 {
        match self {
            Shape::PhysShapeAabb { bounds } => bounds.size().x * bounds.size().y,
            Shape::PhysShapeCircle { radius, .. } => std::f32::consts::PI * radius * radius,
            Shape::PolygonShape { vertices } => {
                let mut doubled = 0.0;
                for (index, a) in vertices.iter().enumerate() {
                    let b = vertices[(index + 1) % vertices.len()];
                    doubled += a.x * b.y - b.x * a.y;
                }
                doubled.abs() / 2.0
            }
        }
    }

    /// Момент инерции фигуры единичной плотности относительно начала координат сущности.
    pub fn unit_inertia(&self) -> f32 {
        match self {
            Shape::PhysShapeAabb { bounds } => {
                let size = bounds.size();
                let center = bounds.center();
                let area = size.x * size.y;
                area * (size.length_squared() / 12.0 + center.length_squared())
            }
            Shape::PhysShapeCircle { radius, position } => {
                let area = std::f32::consts::PI * radius * radius;
                area * (radius * radius / 2.0 + position.length_squared())
            }
            Shape::PolygonShape { vertices } => {
                // Сумма треугольников (начало, a, b): ∫|r|² dA = cross(a, b) · (a·a + a·b + b·b) / 12.
                let mut sum = 0.0;
                for (index, a) in vertices.iter().enumerate() {
                    let b = vertices[(index + 1) % vertices.len()];
                    let cross = a.x * b.y - a.y * b.x;
                    sum += cross * (a.dot(*a) + a.dot(b) + b.dot(b)) / 12.0;
                }
                sum.abs()
            }
        }
    }

    /// Форма в положении тела.
    pub fn at(&self, pose: &Pose) -> WorldShape {
        match self {
            Shape::PhysShapeCircle { radius, position } => WorldShape::Circle {
                center: pose.point(*position),
                radius: *radius,
            },
            Shape::PhysShapeAabb { bounds } => WorldShape::Polygon(
                [
                    Vec2::new(bounds.left(), bounds.bottom()),
                    Vec2::new(bounds.right(), bounds.bottom()),
                    Vec2::new(bounds.right(), bounds.top()),
                    Vec2::new(bounds.left(), bounds.top()),
                ]
                .into_iter()
                .map(|vertex| pose.point(vertex))
                .collect(),
            ),
            Shape::PolygonShape { vertices } => {
                let mut world: SmallVec<[Vec2; 8]> =
                    vertices.iter().map(|vertex| pose.point(*vertex)).collect();
                // Вершины по часовой стрелке разворачиваются: нормали рёбер смотрят наружу.
                let mut doubled = 0.0;
                for (index, a) in world.iter().enumerate() {
                    let b = world[(index + 1) % world.len()];
                    doubled += a.x * b.y - b.x * a.y;
                }
                if doubled < 0.0 {
                    world.reverse();
                }
                WorldShape::Polygon(world)
            }
        }
    }
}

impl WorldShape {
    /// Ограничивающий прямоугольник.
    pub fn aabb(&self) -> Box2 {
        match self {
            WorldShape::Circle { center, radius } => {
                Box2::centered(*center, Vec2::splat(radius * 2.0))
            }
            WorldShape::Polygon(vertices) => {
                let mut bounds = Box2::from_corners(vertices[0], vertices[0]);
                for vertex in &vertices[1..] {
                    bounds = bounds.union(&Box2::from_corners(*vertex, *vertex));
                }
                bounds
            }
        }
    }
}

/// Пересечение двух форм: нормаль от первой ко второй и глубина проникновения.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Manifold {
    pub normal: Vec2,
    pub depth: f32,
    /// Точка контакта.
    pub point: Vec2,
}

fn circle_circle(a: Vec2, ra: f32, b: Vec2, rb: f32) -> Option<Manifold> {
    let delta = b - a;
    let distance = delta.length();
    let depth = ra + rb - distance;
    if depth <= 0.0 {
        return None;
    }
    // Совпавшие центры расталкиваются по оси X: направление не важно, важна определённость.
    let normal = if distance > 1e-6 {
        delta / distance
    } else {
        Vec2::X
    };
    Some(Manifold {
        normal,
        depth,
        point: a + normal * (ra - depth / 2.0),
    })
}

/// Многоугольник (первая форма) и круг (вторая).
fn polygon_circle(vertices: &[Vec2], center: Vec2, radius: f32) -> Option<Manifold> {
    // Ребро с наибольшим расстоянием до центра круга.
    let mut best = f32::NEG_INFINITY;
    let mut best_edge = 0;
    for index in 0..vertices.len() {
        let a = vertices[index];
        let b = vertices[(index + 1) % vertices.len()];
        let edge = b - a;
        let normal = Vec2::new(edge.y, -edge.x).normalize_or_zero();
        let separation = normal.dot(center - a);
        if separation > radius {
            return None;
        }
        if separation > best {
            best = separation;
            best_edge = index;
        }
    }
    let a = vertices[best_edge];
    let b = vertices[(best_edge + 1) % vertices.len()];
    let edge = b - a;
    let normal = Vec2::new(edge.y, -edge.x).normalize_or_zero();
    if best < 0.0 {
        // Центр внутри многоугольника: выталкиваем через ближайшее ребро.
        return Some(Manifold {
            normal,
            depth: radius - best,
            point: center - normal * best,
        });
    }
    // Ближайшая точка ребра: его внутренность или вершина.
    let t = ((center - a).dot(edge) / edge.length_squared()).clamp(0.0, 1.0);
    let closest = a + edge * t;
    let delta = center - closest;
    let distance = delta.length();
    if distance >= radius {
        return None;
    }
    let normal = if distance > 1e-6 {
        delta / distance
    } else {
        normal
    };
    Some(Manifold {
        normal,
        depth: radius - distance,
        point: closest,
    })
}

/// Наименьшее перекрытие по нормалям рёбер `a`: ось и глубина; `None` — формы разделены.
fn least_overlap(a: &[Vec2], b: &[Vec2]) -> Option<(Vec2, f32)> {
    let mut best: Option<(Vec2, f32)> = None;
    for index in 0..a.len() {
        let edge = a[(index + 1) % a.len()] - a[index];
        let normal = Vec2::new(edge.y, -edge.x).normalize_or_zero();
        let max_a = a
            .iter()
            .map(|v| normal.dot(*v))
            .fold(f32::NEG_INFINITY, f32::max);
        let min_b = b
            .iter()
            .map(|v| normal.dot(*v))
            .fold(f32::INFINITY, f32::min);
        let overlap = max_a - min_b;
        if overlap <= 0.0 {
            return None;
        }
        if best.is_none_or(|(_, depth)| overlap < depth) {
            best = Some((normal, overlap));
        }
    }
    best
}

fn polygon_polygon(a: &[Vec2], b: &[Vec2]) -> Option<Manifold> {
    let (normal_a, depth_a) = least_overlap(a, b)?;
    let (normal_b, depth_b) = least_overlap(b, a)?;
    let (normal, depth, incident) = if depth_a <= depth_b {
        (normal_a, depth_a, b)
    } else {
        (-normal_b, depth_b, a)
    };
    // Точка контакта — самая глубокая вершина второй формы вдоль нормали.
    let direction = if depth_a <= depth_b { -normal } else { normal };
    let point = incident
        .iter()
        .copied()
        .max_by(|x, y| direction.dot(*x).total_cmp(&direction.dot(*y)))
        .unwrap_or(Vec2::ZERO);
    Some(Manifold {
        normal,
        depth,
        point,
    })
}

/// Пересечение форм: нормаль направлена от `a` к `b`.
pub fn collide(a: &WorldShape, b: &WorldShape) -> Option<Manifold> {
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
        ) => circle_circle(*ca, *ra, *cb, *rb),
        (WorldShape::Polygon(vertices), WorldShape::Circle { center, radius }) => {
            polygon_circle(vertices, *center, *radius)
        }
        (WorldShape::Circle { center, radius }, WorldShape::Polygon(vertices)) => {
            polygon_circle(vertices, *center, *radius).map(|manifold| Manifold {
                normal: -manifold.normal,
                ..manifold
            })
        }
        (WorldShape::Polygon(a), WorldShape::Polygon(b)) => polygon_polygon(a, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32, y: f32, degrees: f32) -> Pose {
        Pose {
            position: Vec2::new(x, y),
            rotation: Angle::degrees(degrees),
        }
    }

    fn circle(radius: f32) -> Shape {
        Shape::PhysShapeCircle {
            radius,
            position: Vec2::ZERO,
        }
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn circles() {
        let a = circle(0.5).at(&pose(0.0, 0.0, 0.0));
        let b = circle(0.5).at(&pose(0.8, 0.0, 0.0));
        let manifold = collide(&a, &b).unwrap();
        assert!(close(manifold.normal, Vec2::X));
        assert!((manifold.depth - 0.2).abs() < 1e-5);
        assert!(collide(&a, &circle(0.5).at(&pose(1.0, 0.0, 0.0))).is_none());
        assert!(collide(&a, &a).is_some());
    }

    #[test]
    fn circle_against_wall_from_side_corner_and_inside() {
        let wall = Shape::default().at(&pose(0.5, 0.5, 0.0));
        // Сбоку: нормаль от круга к стене.
        let side = collide(&circle(0.35).at(&pose(-0.3, 0.5, 0.0)), &wall).unwrap();
        assert!(close(side.normal, Vec2::X), "{side:?}");
        assert!((side.depth - 0.05).abs() < 1e-5);
        // Обратный порядок форм переворачивает нормаль.
        let reversed = collide(&wall, &circle(0.35).at(&pose(-0.3, 0.5, 0.0))).unwrap();
        assert!(close(reversed.normal, -Vec2::X));
        // Угол: нормаль по диагонали.
        let corner = collide(&circle(0.35).at(&pose(-0.2, -0.2, 0.0)), &wall).unwrap();
        assert!(close(corner.normal, Vec2::ONE.normalize()), "{corner:?}");
        assert!(collide(&circle(0.35).at(&pose(-0.3, -0.3, 0.0)), &wall).is_none());
        // Центр внутри стены: выталкивание через ближайшую грань.
        let inside = collide(&wall, &circle(0.35).at(&pose(0.9, 0.5, 0.0))).unwrap();
        assert!(close(inside.normal, Vec2::X), "{inside:?}");
        assert!((inside.depth - 0.45).abs() < 1e-5);
    }

    #[test]
    fn boxes_axis_aligned_and_rotated() {
        let a = Shape::default().at(&pose(0.0, 0.0, 0.0));
        let b = Shape::default().at(&pose(0.9, 0.2, 0.0));
        let manifold = collide(&a, &b).unwrap();
        assert!(close(manifold.normal, Vec2::X), "{manifold:?}");
        assert!((manifold.depth - 0.1).abs() < 1e-5);
        assert!(collide(&a, &Shape::default().at(&pose(1.01, 0.0, 0.0))).is_none());
        // Повёрнутый на 45° квадрат достаёт углом дальше.
        let rotated = Shape::default().at(&pose(1.1, 0.0, 45.0));
        let manifold = collide(&a, &rotated).unwrap();
        assert!(close(manifold.normal, Vec2::X), "{manifold:?}");
        assert!((manifold.depth - (0.5 + 0.5 * 2f32.sqrt() - 1.1)).abs() < 1e-4);
    }

    #[test]
    fn areas_and_bounds() {
        assert!((circle(1.0).area() - std::f32::consts::PI).abs() < 1e-5);
        assert_eq!(Shape::default().area(), 1.0);
        let triangle = Shape::PolygonShape {
            vertices: vec![Vec2::ZERO, Vec2::new(0.0, 2.0), Vec2::new(2.0, 0.0)],
        };
        assert_eq!(triangle.area(), 2.0);
        // Вершины заданы по часовой стрелке — форма всё равно сталкивается правильно.
        let world = triangle.at(&pose(0.0, 0.0, 0.0));
        let manifold = collide(&world, &circle(0.5).at(&pose(-0.3, 1.0, 0.0))).unwrap();
        assert!(close(manifold.normal, -Vec2::X), "{manifold:?}");
        assert_eq!(world.aabb(), Box2::new(0.0, 0.0, 2.0, 2.0));
        assert_eq!(
            circle(0.5).at(&pose(1.0, 1.0, 0.0)).aabb(),
            Box2::new(0.5, 0.5, 1.5, 1.5)
        );
    }
}
