//! Шаг физики и поиск столкновений. Образец — `SharedPhysicsSystem` и `SharedBroadphaseSystem`
//! Robust @ 1afd692, упрощённые под вид сверху без гравитации.
//!
//! Тела сталкиваются внутри своего грида или космоса карты и своего этажа: у каждого грида
//! и у космоса каждой карты своя сетка поиска. В расчёте участвуют только бодрствующие тела,
//! поэтому тысячи стен и лежащих предметов не стоят ничего, пока рядом никто не движется.

use std::collections::{HashMap, HashSet};

use astrocore_ecs::{EventBus, EventWorldExt, GameTime, Paused};
use astrocore_map::{Transform, TransformWorldExt};
use astrocore_math::{Angle, Box2, Vec2};
use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::Without;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use smallvec::SmallVec;

use crate::body::{BodyType, Fixtures, Physics};
use crate::shapes::{Pose, collide};

/// Ниже этой скорости (тайлов в секунду) тело считается неподвижным.
pub const SLEEP_SPEED: f32 = 0.01;
/// Столько секунд неподвижности — и тело засыпает.
pub const TIME_TO_SLEEP: f32 = 0.2;
/// Проникновение, которое не исправляется: тела остаются в касании.
pub const LINEAR_SLOP: f32 = 0.005;
/// Наибольшее перемещение за один подшаг: быстрые тела не пролетают сквозь стены.
const MAX_SUBSTEP_TRAVEL: f32 = 0.25;
const MAX_SUBSTEPS: usize = 8;
/// Проходов расталкивания за подшаг.
const SOLVER_ITERATIONS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellKey {
    root: Entity,
    x: i32,
    y: i32,
    z: i16,
}

/// Клетки сетки поиска, которые занимает тело.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
struct Proxy {
    root: Entity,
    z: i16,
    min: (i32, i32),
    max: (i32, i32),
}

impl Proxy {
    fn cells(self) -> impl Iterator<Item = CellKey> {
        (self.min.0..=self.max.0).flat_map(move |x| {
            (self.min.1..=self.max.1).map(move |y| CellKey {
                root: self.root,
                x,
                y,
                z: self.z,
            })
        })
    }
}

/// Пара касающихся фикстур: сущности по возрастанию и номера их фикстур.
type Pair = (Entity, u8, Entity, u8);

/// Сетка поиска столкновений и текущие касания. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct Broadphase {
    cells: HashMap<CellKey, SmallVec<[Entity; 4]>>,
    /// Касающиеся пары → столкновение запрещено обработчиком `PreventCollide`.
    contacts: HashMap<Pair, bool>,
    /// Сущность → её пары.
    pairs_of: HashMap<Entity, SmallVec<[Pair; 4]>>,
}

impl Broadphase {
    /// Число касающихся пар фикстур.
    pub fn contact_count(&self) -> usize {
        self.contacts.len()
    }

    fn forget_pair(&mut self, pair: Pair) {
        self.contacts.remove(&pair);
        for entity in [pair.0, pair.2] {
            if let Some(pairs) = self.pairs_of.get_mut(&entity) {
                pairs.retain(|p| *p != pair);
                if pairs.is_empty() {
                    self.pairs_of.remove(&entity);
                }
            }
        }
    }
}

/// Фикстуры двух тел начали касаться. Поднимается на обоих телах.
#[derive(Debug, Clone)]
pub struct StartCollide {
    pub our_entity: Entity,
    pub other_entity: Entity,
    pub our_fixture: String,
    pub other_fixture: String,
}

/// Фикстуры двух тел перестали касаться. Поднимается на обоих телах.
#[derive(Debug, Clone)]
pub struct EndCollide {
    pub our_entity: Entity,
    pub other_entity: Entity,
    pub our_fixture: String,
    pub other_fixture: String,
}

/// Можно ли телам столкнуться: обработчик отменяет столкновение, и тела проходят друг
/// сквозь друга, пока касаются. Поднимается на обоих телах в начале касания.
#[derive(Debug, Clone)]
pub struct PreventCollide {
    pub our_entity: Entity,
    pub other_entity: Entity,
    pub our_fixture: String,
    pub other_fixture: String,
    pub cancelled: bool,
}

/// Тело уснуло: перестало двигаться и выпало из расчёта (`PhysicsSleepEvent`).
/// Поднимается на теле.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsSleep {
    pub entity: Entity,
}

/// Положение тела в его гриде или космосе карты: корень, этаж и поза. `None` — тело не
/// стоит прямо на гриде или карте (лежит в контейнере) и в столкновениях не участвует.
pub(crate) fn placement(world: &World, entity: Entity) -> Option<(Entity, i16, Pose)> {
    let transform = world.get::<Transform>(entity)?;
    let root = transform.grid().or(transform.map())?;
    (world.parent(entity) == Some(root)).then_some((
        root,
        transform.level(),
        Pose {
            position: transform.local_position(),
            rotation: transform.local_rotation(),
        },
    ))
}

fn fixture_name(fixtures: &Fixtures, index: u8) -> String {
    fixtures
        .fixtures
        .keys()
        .nth(usize::from(index))
        .cloned()
        .unwrap_or_default()
}

fn bounds_of(fixtures: &Fixtures, pose: &Pose) -> Option<Box2> {
    fixtures
        .fixtures
        .values()
        .map(|fixture| fixture.shape.at(pose).aabb())
        .reduce(|a, b| a.union(&b))
}

fn cell_range(bounds: &Box2) -> ((i32, i32), (i32, i32)) {
    (
        (bounds.left().floor() as i32, bounds.bottom().floor() as i32),
        (bounds.right().floor() as i32, bounds.top().floor() as i32),
    )
}

fn raise_end(world: &mut World, pair: Pair) {
    let names = |world: &World, entity: Entity, index: u8| {
        world
            .get::<Fixtures>(entity)
            .map(|f| fixture_name(f, index))
            .unwrap_or_default()
    };
    let (a, fa, b, fb) = pair;
    let (name_a, name_b) = (names(world, a, fa), names(world, b, fb));
    world.raise(
        a,
        &mut EndCollide {
            our_entity: a,
            other_entity: b,
            our_fixture: name_a.clone(),
            other_fixture: name_b.clone(),
        },
    );
    world.raise(
        b,
        &mut EndCollide {
            our_entity: b,
            other_entity: a,
            our_fixture: name_b,
            other_fixture: name_a,
        },
    );
}

/// Обновляет клетки тела в сетке поиска по его текущему положению.
pub(crate) fn update_proxy(world: &mut World, entity: Entity) {
    let proxy = world
        .get::<Physics>(entity)
        .filter(|body| body.can_collide)
        .and_then(|_| world.get::<Fixtures>(entity))
        .and_then(|fixtures| {
            let (root, z, pose) = placement(world, entity)?;
            let (min, max) = cell_range(&bounds_of(fixtures, &pose)?);
            Some(Proxy { root, z, min, max })
        });
    let previous = world.get::<Proxy>(entity).copied();
    if previous == proxy {
        return;
    }
    let mut broadphase = world.get_resource_or_insert_with(Broadphase::default);
    if let Some(previous) = previous {
        for cell in previous.cells() {
            if let Some(entities) = broadphase.cells.get_mut(&cell) {
                entities.retain(|e| *e != entity);
                if entities.is_empty() {
                    broadphase.cells.remove(&cell);
                }
            }
        }
    }
    if let Some(proxy) = proxy {
        for cell in proxy.cells() {
            broadphase.cells.entry(cell).or_default().push(entity);
        }
    }
    match proxy {
        Some(proxy) => {
            world.entity_mut(entity).insert(proxy);
        }
        None => {
            // Тело выбыло из столкновений: его касания закончились.
            let pairs = world
                .resource_mut::<Broadphase>()
                .pairs_of
                .remove(&entity)
                .unwrap_or_default();
            for pair in pairs {
                world.resource_mut::<Broadphase>().forget_pair(pair);
                raise_end(world, pair);
            }
            if let Ok(mut target) = world.get_entity_mut(entity) {
                target.remove::<Proxy>();
            }
        }
    }
}

/// Тело в расчёте шага.
#[derive(Debug, Clone)]
struct Active {
    entity: Entity,
    root: Entity,
    z: i16,
    position: Vec2,
    rotation: Angle,
    velocity: Vec2,
    angular: f32,
    inv_mass: f32,
    inv_inertia: f32,
    body_type: BodyType,
    damping: f32,
    angular_damping: f32,
    fixed_rotation: bool,
    collides: bool,
    start_position: Vec2,
    start_rotation: Angle,
}

impl Active {
    fn load(world: &World, entity: Entity) -> Option<Self> {
        let body = world.get::<Physics>(entity)?;
        let (root, z, pose) = placement(world, entity)?;
        Some(Self {
            entity,
            root,
            z,
            position: pose.position,
            rotation: pose.rotation,
            velocity: body.linear_velocity,
            angular: body.angular_velocity,
            inv_mass: body.inv_mass(),
            inv_inertia: body.inv_inertia(),
            body_type: body.body_type,
            damping: body.linear_damping,
            angular_damping: body.angular_damping,
            fixed_rotation: body.fixed_rotation,
            collides: body.can_collide && world.get::<Fixtures>(entity).is_some(),
            start_position: pose.position,
            start_rotation: pose.rotation,
        })
    }

    fn pose(&self) -> Pose {
        Pose {
            position: self.position,
            rotation: self.rotation,
        }
    }
}

/// Найденное перекрытие фикстур.
struct Hit {
    a: usize,
    /// Второе тело: номер среди активных или неподвижная сущность.
    b: Result<usize, Entity>,
    pair: Pair,
    /// Нормаль от первого тела ко второму.
    normal: Vec2,
    depth: f32,
    /// Точка касания в координатах корня.
    point: Vec2,
    hard: bool,
    restitution: f32,
    /// Трение пары (`MixFriction` Box2D: корень из произведения).
    friction: f32,
    /// Второе тело спит и только что разбужено: касание решается со следующего прохода,
    /// когда оно войдёт в расчёт со своей массой.
    waking: bool,
}

/// Ищет перекрытия активных тел с окружением.
#[allow(clippy::too_many_arguments)]
fn detect(
    world: &World,
    broadphase: &Broadphase,
    bodies: &[Active],
    index_of: &HashMap<Entity, usize>,
    margin: f32,
    no_collide: &HashSet<(Entity, Entity)>,
    hits: &mut Vec<Hit>,
    wake: &mut Vec<Entity>,
) {
    hits.clear();
    let mut checked: HashSet<(Entity, Entity)> = HashSet::new();
    for (a_index, a) in bodies.iter().enumerate() {
        if !a.collides {
            continue;
        }
        let Some(a_fixtures) = world.get::<Fixtures>(a.entity) else {
            continue;
        };
        let a_pose = a.pose();
        let Some(bounds) = bounds_of(a_fixtures, &a_pose) else {
            continue;
        };
        let (min, max) = cell_range(&bounds.enlarged(margin));
        for x in min.0..=max.0 {
            for y in min.1..=max.1 {
                let key = CellKey {
                    root: a.root,
                    x,
                    y,
                    z: a.z,
                };
                let Some(candidates) = broadphase.cells.get(&key) else {
                    continue;
                };
                for &other in candidates {
                    if other == a.entity {
                        continue;
                    }
                    let ordered = if a.entity < other {
                        (a.entity, other)
                    } else {
                        (other, a.entity)
                    };
                    if !checked.insert(ordered) || no_collide.contains(&ordered) {
                        continue;
                    }
                    let b_index = index_of.get(&other).copied();
                    let (b_pose, b_movable) = match b_index {
                        Some(index) => (bodies[index].pose(), bodies[index].inv_mass > 0.0),
                        None => {
                            let Some((_, _, pose)) = placement(world, other) else {
                                continue;
                            };
                            let movable = world
                                .get::<Physics>(other)
                                .is_some_and(|body| body.inv_mass() > 0.0);
                            (pose, movable)
                        }
                    };
                    let Some(b_fixtures) = world.get::<Fixtures>(other) else {
                        continue;
                    };
                    for (fa, fixture_a) in a_fixtures.fixtures.values().enumerate() {
                        let shape_a = fixture_a.shape.at(&a_pose);
                        for (fb, fixture_b) in b_fixtures.fixtures.values().enumerate() {
                            if !fixture_a.should_collide(fixture_b) {
                                continue;
                            }
                            let Some(manifold) = collide(&shape_a, &fixture_b.shape.at(&b_pose))
                            else {
                                continue;
                            };
                            let pair = if a.entity < other {
                                (a.entity, fa as u8, other, fb as u8)
                            } else {
                                (other, fb as u8, a.entity, fa as u8)
                            };
                            let hard = fixture_a.hard && fixture_b.hard;
                            // Спящее подвижное тело от твёрдого удара просыпается.
                            let waking = hard && b_index.is_none() && b_movable;
                            if waking {
                                wake.push(other);
                            }
                            hits.push(Hit {
                                a: a_index,
                                b: b_index.ok_or(other),
                                pair,
                                normal: manifold.normal,
                                depth: manifold.depth,
                                point: manifold.point,
                                hard,
                                restitution: fixture_a.restitution.max(fixture_b.restitution),
                                friction: (fixture_a.friction * fixture_b.friction).sqrt(),
                                waking,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Решает, запрещено ли столкновение новой пары, и запоминает касание.
fn admit(world: &mut World, pair: Pair) -> bool {
    if let Some(&prevented) = world.resource::<Broadphase>().contacts.get(&pair) {
        return prevented;
    }
    let (a, fa, b, fb) = pair;
    let name = |world: &World, entity: Entity, index: u8| {
        world
            .get::<Fixtures>(entity)
            .map(|f| fixture_name(f, index))
            .unwrap_or_default()
    };
    let (name_a, name_b) = (name(world, a, fa), name(world, b, fb));
    let mut prevented = false;
    if world
        .resource::<EventBus>()
        .has_directed::<PreventCollide>()
    {
        for (our, other, our_fixture, other_fixture) in
            [(a, b, &name_a, &name_b), (b, a, &name_b, &name_a)]
        {
            let mut event = PreventCollide {
                our_entity: our,
                other_entity: other,
                our_fixture: our_fixture.clone(),
                other_fixture: other_fixture.clone(),
                cancelled: false,
            };
            world.raise(our, &mut event);
            prevented |= event.cancelled;
        }
    }
    let mut broadphase = world.resource_mut::<Broadphase>();
    broadphase.contacts.insert(pair, prevented);
    broadphase.pairs_of.entry(a).or_default().push(pair);
    broadphase.pairs_of.entry(b).or_default().push(pair);
    for (our, other, our_fixture, other_fixture) in
        [(a, b, &name_a, &name_b), (b, a, &name_b, &name_a)]
    {
        world.raise(
            our,
            &mut StartCollide {
                our_entity: our,
                other_entity: other,
                our_fixture: our_fixture.clone(),
                other_fixture: other_fixture.clone(),
            },
        );
    }
    prevented
}

/// Положение тела сочленения: активное — из расчёта, иное — из мира, неподвижное.
fn joint_body(
    world: &World,
    bodies: &[Active],
    index_of: &HashMap<Entity, usize>,
    entity: Entity,
) -> Option<(Option<usize>, Entity, i16, Pose, Vec2, f32)> {
    match index_of.get(&entity) {
        Some(&index) => {
            let body = &bodies[index];
            let inv = if body.body_type == BodyType::Dynamic {
                body.inv_mass
            } else {
                0.0
            };
            Some((
                Some(index),
                body.root,
                body.z,
                body.pose(),
                body.velocity,
                inv,
            ))
        }
        None => {
            let (root, z, pose) = placement(world, entity)?;
            Some((None, root, z, pose, Vec2::ZERO, 0.0))
        }
    }
}

/// Дистанционные сочленения (`DistanceJoint.SolvePositionConstraints` и
/// `SolveVelocityConstraints` Box2D, без вращения): вышедшие за пределы тела
/// возвращаются, скорость вдоль сочленения гасится; при жёсткости — пружина.
fn solve_joints(
    world: &World,
    joints: &[crate::joint::Joint],
    bodies: &mut [Active],
    index_of: &HashMap<Entity, usize>,
    step: f32,
) {
    for joint in joints {
        let Some(distance) = joint.distance() else {
            continue;
        };
        let (Some(a), Some(b)) = (
            joint_body(world, bodies, index_of, joint.body_a),
            joint_body(world, bodies, index_of, joint.body_b),
        ) else {
            continue;
        };
        let (a_index, a_root, a_z, a_pose, a_velocity, inv_a) = a;
        let (b_index, b_root, b_z, b_pose, b_velocity, inv_b) = b;
        if a_root != b_root || a_z != b_z {
            continue;
        }
        let total = inv_a + inv_b;
        if total <= 0.0 {
            continue;
        }
        let point_a = a_pose.point(joint.anchor_a);
        let point_b = b_pose.point(joint.anchor_b);
        let delta = point_b - point_a;
        let length = delta.length();
        if length <= f32::EPSILON {
            continue;
        }
        let axis = delta / length;
        let approach = (b_velocity - a_velocity).dot(axis);
        let (correction, impulse) = if distance.stiffness > 0.0 {
            let force =
                distance.stiffness * (length - distance.length) + distance.damping * approach;
            (0.0, -force * step)
        } else if length > distance.max_length {
            (
                length - distance.max_length,
                if approach > 0.0 {
                    -approach / total
                } else {
                    0.0
                },
            )
        } else if length < distance.min_length {
            (
                length - distance.min_length,
                if approach < 0.0 {
                    -approach / total
                } else {
                    0.0
                },
            )
        } else {
            continue;
        };
        if let Some(index) = a_index {
            let body = &mut bodies[index];
            body.position += axis * correction * (inv_a / total);
            body.velocity -= axis * impulse * inv_a;
        }
        if let Some(index) = b_index {
            let body = &mut bodies[index];
            body.position -= axis * correction * (inv_b / total);
            body.velocity += axis * impulse * inv_b;
        }
    }
}

/// Шаг физики: `only` ограничивает расчёт одним телом (предсказание на клиенте), остальные
/// тела для него неподвижны.
pub fn step_physics(world: &mut World, dt: f32, only: Option<Entity>) {
    world.init_resource::<Broadphase>();
    let mut entities: Vec<Entity> = match only {
        Some(entity) => vec![entity],
        None => world
            .query_filtered::<(Entity, &Physics), Without<Paused>>()
            .iter(world)
            .filter(|(_, body)| body.awake && body.body_type != BodyType::Static)
            .map(|(entity, _)| entity)
            .collect(),
    };
    entities.sort_unstable();
    let mut bodies: Vec<Active> = entities
        .iter()
        .filter_map(|entity| Active::load(world, *entity))
        .collect();
    if bodies.is_empty() {
        return;
    }

    // Затухание скорости (как в Box2D): v /= 1 + dt · damping.
    let mut top_speed = 0.0f32;
    for body in &mut bodies {
        if body.body_type == BodyType::Dynamic {
            body.velocity /= 1.0 + dt * body.damping;
            body.angular /= 1.0 + dt * body.angular_damping;
        }
        top_speed = top_speed.max(body.velocity.length());
    }
    let substeps = ((top_speed * dt / MAX_SUBSTEP_TRAVEL).ceil() as usize).clamp(1, MAX_SUBSTEPS);
    let step = dt / substeps as f32;
    let mut index_of: HashMap<Entity, usize> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| (body.entity, index))
        .collect();
    let margin = top_speed * dt + 0.1;
    // Сочленения активных тел; спящее свободное тело на сочленении просыпается.
    let joints = crate::joint::active_joints(world, bodies.iter().map(|body| body.entity));
    if only.is_none() {
        for joint in &joints {
            for entity in [joint.body_a, joint.body_b] {
                let dynamic = world
                    .get::<Physics>(entity)
                    .is_some_and(|body| body.body_type == BodyType::Dynamic);
                if dynamic
                    && !index_of.contains_key(&entity)
                    && let Some(body) = Active::load(world, entity)
                {
                    index_of.insert(entity, bodies.len());
                    bodies.push(body);
                }
            }
        }
    }
    let no_collide: HashSet<(Entity, Entity)> = joints
        .iter()
        .filter(|joint| !joint.collide_connected)
        .map(|joint| {
            if joint.body_a < joint.body_b {
                (joint.body_a, joint.body_b)
            } else {
                (joint.body_b, joint.body_a)
            }
        })
        .collect();
    let mut hits = Vec::new();
    let mut wake = Vec::new();
    let mut touching: HashSet<Pair> = HashSet::new();

    for _ in 0..substeps {
        for body in &mut bodies {
            body.position += body.velocity * step;
            if !body.fixed_rotation {
                body.rotation = body.rotation + Angle(body.angular * step);
            }
        }
        solve_joints(world, &joints, &mut bodies, &index_of, step);
        touching.clear();
        for iteration in 0..SOLVER_ITERATIONS {
            world.resource_scope(|world, broadphase: bevy_ecs::world::Mut<Broadphase>| {
                detect(
                    world,
                    &broadphase,
                    &bodies,
                    &index_of,
                    margin,
                    &no_collide,
                    &mut hits,
                    &mut wake,
                );
            });
            // Разбуженные тела вступают в расчёт со следующего прохода.
            if only.is_none() {
                for entity in wake.drain(..) {
                    if !index_of.contains_key(&entity)
                        && let Some(body) = Active::load(world, entity)
                    {
                        index_of.insert(entity, bodies.len());
                        bodies.push(body);
                    }
                }
            } else {
                wake.clear();
            }
            let mut resolved = false;
            // За проход у каждого тела решается только самое глубокое твёрдое касание:
            // после него мнимые касания с углами соседних стен исчезают, и тело скользит
            // вдоль стены из отдельных тайлов без рывков.
            let mut deepest: HashMap<usize, usize> = HashMap::new();
            for (hit_index, hit) in hits.iter().enumerate() {
                if iteration == 0 {
                    touching.insert(hit.pair);
                }
                let prevented = admit(world, hit.pair);
                if !hit.hard || prevented || hit.depth <= LINEAR_SLOP {
                    continue;
                }
                deepest
                    .entry(hit.a)
                    .and_modify(|best| {
                        if hit.depth > hits[*best].depth {
                            *best = hit_index;
                        }
                    })
                    .or_insert(hit_index);
            }
            let mut chosen: Vec<usize> = deepest.into_values().collect();
            chosen.sort_unstable();
            for hit_index in chosen {
                let hit = &hits[hit_index];
                if hit.waking && only.is_none() {
                    resolved = true;
                    continue;
                }
                let a_body = &bodies[hit.a];
                let (inv_a, inv_ia, velocity_a, omega_a, center_a) = (
                    a_body.inv_mass,
                    a_body.inv_inertia,
                    a_body.velocity,
                    a_body.angular,
                    a_body.position,
                );
                let a_dynamic = a_body.body_type == BodyType::Dynamic;
                let (inv_b, inv_ib, velocity_b, omega_b, center_b, b_dynamic) = match hit.b {
                    Ok(index) => {
                        let b = &bodies[index];
                        (
                            b.inv_mass,
                            b.inv_inertia,
                            b.velocity,
                            b.angular,
                            b.position,
                            b.body_type == BodyType::Dynamic,
                        )
                    }
                    Err(_) => (0.0, 0.0, Vec2::ZERO, 0.0, hit.point, false),
                };
                let total = inv_a + inv_b;
                if total <= 0.0 {
                    continue;
                }
                resolved = true;
                // Расталкивание по нормали пропорционально обратным массам.
                let correction = hit.normal * (hit.depth - LINEAR_SLOP);
                // Импульсы в точке касания (Box2D): по нормали гасят сближение, по касательной —
                // трение; плечо от центра тела к точке раскручивает тело.
                let (ra, rb) = (hit.point - center_a, hit.point - center_b);
                let cross = |r: Vec2, v: Vec2| r.x * v.y - r.y * v.x;
                let spin = |omega: f32, r: Vec2| Vec2::new(-omega * r.y, omega * r.x);
                let relative = |va: Vec2, wa: f32, vb: Vec2, wb: f32| {
                    (vb + spin(wb, rb)) - (va + spin(wa, ra))
                };
                let effective = |axis: Vec2| {
                    total + inv_ia * cross(ra, axis).powi(2) + inv_ib * cross(rb, axis).powi(2)
                };
                let approach = relative(velocity_a, omega_a, velocity_b, omega_b).dot(hit.normal);
                let normal_impulse = if approach < 0.0 {
                    -(1.0 + hit.restitution) * approach / effective(hit.normal)
                } else {
                    0.0
                };
                let mut impulse = hit.normal * normal_impulse;
                // Трение действует, если в паре есть свободное тело: персонажей ведёт
                // управление, и о стены они не тормозят.
                if (a_dynamic || b_dynamic) && hit.friction > 0.0 && normal_impulse > 0.0 {
                    let (va, wa) = (
                        velocity_a - impulse * inv_a,
                        omega_a - inv_ia * cross(ra, impulse),
                    );
                    let (vb, wb) = (
                        velocity_b + impulse * inv_b,
                        omega_b + inv_ib * cross(rb, impulse),
                    );
                    let tangent = Vec2::new(-hit.normal.y, hit.normal.x);
                    let slide = relative(va, wa, vb, wb).dot(tangent);
                    let limit = hit.friction * normal_impulse;
                    let friction_impulse = (-slide / effective(tangent)).clamp(-limit, limit);
                    impulse += tangent * friction_impulse;
                }
                let a = &mut bodies[hit.a];
                a.position -= correction * (inv_a / total);
                a.velocity -= impulse * inv_a;
                a.angular -= inv_ia * cross(ra, impulse);
                if let Ok(index) = hit.b {
                    let b = &mut bodies[index];
                    b.position += correction * (inv_b / total);
                    b.velocity += impulse * inv_b;
                    b.angular += inv_ib * cross(rb, impulse);
                }
            }
            if !resolved {
                break;
            }
        }
    }

    // Касания, которых больше нет.
    let ended: Vec<Pair> = {
        let broadphase = world.resource::<Broadphase>();
        bodies
            .iter()
            .filter_map(|body| broadphase.pairs_of.get(&body.entity))
            .flatten()
            .filter(|pair| !touching.contains(*pair))
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    };
    for pair in ended {
        world.resource_mut::<Broadphase>().forget_pair(pair);
        raise_end(world, pair);
    }

    // Запись результата в мир.
    let mut asleep = Vec::new();
    for body in &bodies {
        if world.get_entity(body.entity).is_err() {
            continue;
        }
        if body.position != body.start_position || body.rotation != body.start_rotation {
            let Some(parent) = world.parent(body.entity) else {
                continue;
            };
            let _ = world.set_coordinates(
                body.entity,
                astrocore_map::EntityCoordinates::on_level(parent, body.position, body.z),
                Some(body.rotation),
            );
        }
        // Тело перешло на другой грид: скорость хранится в осях родителя, поэтому
        // поворачивается на разницу поворотов старого и нового родителя.
        let mut body_velocity = body.velocity;
        if let Some(new_root) = world.parent(body.entity).filter(|root| *root != body.root) {
            let turn = |root| world.world_transform(root).map_or(0.0, |t| t.rotation.0);
            let angle = turn(body.root) - turn(new_root);
            if angle != 0.0 {
                let (sin, cos) = angle.sin_cos();
                body_velocity = Vec2::new(
                    body_velocity.x * cos - body_velocity.y * sin,
                    body_velocity.x * sin + body_velocity.y * cos,
                );
            }
        }
        let Some(mut state) = world.get_mut::<Physics>(body.entity) else {
            continue;
        };
        let resting =
            body.velocity.length_squared() < SLEEP_SPEED * SLEEP_SPEED && body.angular.abs() < 0.01;
        let sleep_time = if resting { state.sleep_time + dt } else { 0.0 };
        let falls_asleep = resting && state.sleeping_allowed && sleep_time >= TIME_TO_SLEEP;
        let (velocity, angular, awake) = if falls_asleep {
            (Vec2::ZERO, 0.0, false)
        } else {
            (body_velocity, body.angular, true)
        };
        // Неизменившееся тело не помечается изменённым: оно не уходит клиентам заново.
        if state.linear_velocity != velocity
            || state.angular_velocity != angular
            || state.awake != awake
        {
            state.linear_velocity = velocity;
            state.angular_velocity = angular;
            state.awake = awake;
        }
        if state.sleep_time != sleep_time {
            state.bypass_change_detection().sleep_time = sleep_time;
        }
        if falls_asleep {
            asleep.push(body.entity);
        }
    }
    for entity in asleep {
        world.raise(entity, &mut PhysicsSleep { entity });
    }
}

/// Система шага физики: этап `PostUpdate`.
pub(crate) fn physics_system(world: &mut World) {
    let dt = world.resource::<GameTime>().frame_time();
    step_physics(world, dt, None);
}

/// Управление телами на `World`.
pub trait PhysicsWorldExt {
    /// Задаёт скорость тела и будит его.
    fn set_linear_velocity(&mut self, entity: Entity, velocity: Vec2);

    /// Прикладывает импульс: скорость меняется на импульс, делённый на массу.
    fn apply_linear_impulse(&mut self, entity: Entity, impulse: Vec2);

    /// Прикладывает вращающий импульс: угловая скорость меняется на импульс, делённый на
    /// момент инерции.
    fn apply_angular_impulse(&mut self, entity: Entity, impulse: f32);

    /// Задаёт угловую скорость и будит тело.
    fn set_angular_velocity(&mut self, entity: Entity, angular: f32);

    /// Задаёт положение тела относительно пола: в воздухе трение о пол не действует.
    fn set_body_status(&mut self, entity: Entity, status: crate::body::BodyStatus);

    /// Задаёт затухание скорости тела.
    fn set_damping(&mut self, entity: Entity, linear: f32, angular: f32);

    /// Пересчитывает массу и место в сетке поиска после изменения фикстур во время игры.
    fn refresh_fixtures(&mut self, entity: Entity);

    /// Меняет вид тела.
    fn set_body_type(&mut self, entity: Entity, body_type: BodyType);

    /// Включает или выключает столкновения тела.
    fn set_can_collide(&mut self, entity: Entity, can_collide: bool);

    /// Будит тело.
    fn wake_body(&mut self, entity: Entity);

    /// Сущности, фикстуры которых пересекают прямоугольник в координатах грида или карты
    /// `root` на этаже `z`.
    fn entities_intersecting(&self, root: Entity, z: i16, bounds: Box2) -> Vec<Entity>;
}

impl PhysicsWorldExt for World {
    fn set_linear_velocity(&mut self, entity: Entity, velocity: Vec2) {
        let Some(mut body) = self.get_mut::<Physics>(entity) else {
            return;
        };
        if body.body_type == BodyType::Static || body.linear_velocity == velocity {
            return;
        }
        body.linear_velocity = velocity;
        if velocity != Vec2::ZERO {
            body.awake = true;
            body.sleep_time = 0.0;
        }
    }

    fn apply_linear_impulse(&mut self, entity: Entity, impulse: Vec2) {
        let Some(body) = self.get::<Physics>(entity) else {
            return;
        };
        let velocity = body.linear_velocity + impulse * body.inv_mass();
        self.set_linear_velocity(entity, velocity);
    }

    fn apply_angular_impulse(&mut self, entity: Entity, impulse: f32) {
        let Some(body) = self.get::<Physics>(entity) else {
            return;
        };
        let angular = body.angular_velocity + impulse * body.inv_inertia();
        self.set_angular_velocity(entity, angular);
    }

    fn set_angular_velocity(&mut self, entity: Entity, angular: f32) {
        let Some(mut body) = self.get_mut::<Physics>(entity) else {
            return;
        };
        if body.body_type == BodyType::Static || body.angular_velocity == angular {
            return;
        }
        body.angular_velocity = angular;
        if angular != 0.0 {
            body.awake = true;
            body.sleep_time = 0.0;
        }
    }

    fn set_body_status(&mut self, entity: Entity, status: crate::body::BodyStatus) {
        if let Some(mut body) = self.get_mut::<Physics>(entity)
            && body.body_status != status
        {
            body.body_status = status;
        }
    }

    fn set_damping(&mut self, entity: Entity, linear: f32, angular: f32) {
        if let Some(mut body) = self.get_mut::<Physics>(entity)
            && (body.linear_damping != linear || body.angular_damping != angular)
        {
            // Затухание меняется каждый такт и клиенту не нужно: не помечаем изменённым.
            let body = body.bypass_change_detection();
            body.linear_damping = linear;
            body.angular_damping = angular;
        }
    }

    fn refresh_fixtures(&mut self, entity: Entity) {
        crate::update_mass(self, entity);
        update_proxy(self, entity);
    }

    fn set_body_type(&mut self, entity: Entity, body_type: BodyType) {
        let Some(mut body) = self.get_mut::<Physics>(entity) else {
            return;
        };
        if body.body_type == body_type {
            return;
        }
        body.body_type = body_type;
        if body_type == BodyType::Static {
            body.linear_velocity = Vec2::ZERO;
            body.angular_velocity = 0.0;
            body.awake = false;
        }
    }

    fn set_can_collide(&mut self, entity: Entity, can_collide: bool) {
        let Some(mut body) = self.get_mut::<Physics>(entity) else {
            return;
        };
        if body.can_collide == can_collide {
            return;
        }
        body.can_collide = can_collide;
        update_proxy(self, entity);
    }

    fn wake_body(&mut self, entity: Entity) {
        if let Some(mut body) = self.get_mut::<Physics>(entity)
            && body.body_type != BodyType::Static
            && !body.awake
        {
            body.awake = true;
            body.sleep_time = 0.0;
        }
    }

    fn entities_intersecting(&self, root: Entity, z: i16, bounds: Box2) -> Vec<Entity> {
        let Some(broadphase) = self.get_resource::<Broadphase>() else {
            return Vec::new();
        };
        let (min, max) = cell_range(&bounds);
        let mut found = Vec::new();
        for x in min.0..=max.0 {
            for y in min.1..=max.1 {
                let Some(entities) = broadphase.cells.get(&CellKey { root, x, y, z }) else {
                    continue;
                };
                for &entity in entities {
                    if found.contains(&entity) {
                        continue;
                    }
                    let overlaps = placement(self, entity)
                        .zip(self.get::<Fixtures>(entity))
                        .and_then(|((_, _, pose), fixtures)| bounds_of(fixtures, &pose))
                        .is_some_and(|own| own.intersects(&bounds));
                    if overlaps {
                        found.push(entity);
                    }
                }
            }
        }
        found
    }
}
