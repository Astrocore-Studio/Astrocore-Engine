use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use astrocore_api::PhysicsQuery;
use astrocore_core::ObjectId;
use astrocore_math::Aabb;
use astrocore_math::glam::Vec3;
use astrocore_physics::{BoxWorld, Contact, LAYER_ALL, RayHit, ShapeHit, SpatialQuery};
use avian3d::prelude::{SpatialQuery as AvianSpatialQuery, *};
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

/// Шаг физики по умолчанию, секунды (фиксированный, как у сервера).
pub const DEFAULT_DT: f32 = 1.0 / 60.0;

#[derive(Component)]
struct Tagged(ObjectId);

struct Body {
    entity: Entity,
    aabb: Aabb,
}

/// Физический мир на Avian3D. Статика приходит из коллизии мира Astrocore (`BoxWorld`), слои
/// совпадают с масками движка; поверх добавлены динамические и кинематические тела — то, чего
/// нет у собственного слоя. Реализует `PhysicsQuery`, поэтому игровой код не зависит от выбора.
pub struct AvianWorld {
    app: RefCell<App>,
    bodies: HashMap<ObjectId, Body>,
    dt: f32,
}

fn to_bevy(v: Vec3) -> bevy::math::Vec3 {
    bevy::math::Vec3::new(v.x, v.y, v.z)
}

fn from_bevy(v: bevy::math::Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

impl Default for AvianWorld {
    fn default() -> Self {
        Self::new(DEFAULT_DT)
    }
}

impl AvianWorld {
    pub fn new(dt: f32) -> Self {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .insert_resource(Time::<Fixed>::from_seconds(f64::from(dt)))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                dt,
            )));
        // Плагины Avian достраиваются в `finish`: без него ресурсы физики не существуют.
        app.finish();
        app.cleanup();
        // Первые обновления инициализируют часы; дальше каждый `update` — один шаг физики.
        app.update();
        app.update();
        Self {
            app: RefCell::new(app),
            bodies: HashMap::new(),
            dt,
        }
    }

    /// Построить статику по коллизии мира.
    pub fn from_box_world(bw: &BoxWorld) -> Self {
        let mut w = Self::default();
        w.sync_statics(bw);
        w
    }

    fn layers(memberships: u32) -> CollisionLayers {
        CollisionLayers::new(LayerMask(memberships), LayerMask::ALL)
    }

    fn cuboid(aabb: &Aabb) -> Collider {
        let s = aabb.size().max(Vec3::splat(1e-3));
        Collider::cuboid(s.x, s.y, s.z)
    }

    fn spawn_box(&mut self, id: ObjectId, aabb: Aabb, body: RigidBody, layer: u32) {
        let c = aabb.center();
        let entity = self
            .app
            .get_mut()
            .world_mut()
            .spawn((
                body,
                Self::cuboid(&aabb),
                Self::layers(layer),
                Transform::from_translation(to_bevy(c)),
                Tagged(id),
            ))
            .id();
        self.bodies.insert(id, Body { entity, aabb });
    }

    /// Привести статику в соответствие с `BoxWorld`: новое добавляется, изменённое пересоздаётся,
    /// исчезнувшее удаляется. Динамические тела не затрагиваются.
    pub fn sync_statics(&mut self, bw: &BoxWorld) {
        let want: HashMap<ObjectId, (Aabb, u32)> = bw
            .static_boxes()
            .map(|(id, b)| (*id, (*b, bw.layer_of(*id, true))))
            .chain(
                bw.dynamic_boxes()
                    .map(|(id, b)| (id, (b, bw.layer_of(id, false)))),
            )
            .collect();
        let stale: Vec<ObjectId> = self
            .bodies
            .iter()
            .filter(|(id, b)| {
                let is_static = self
                    .app
                    .borrow()
                    .world()
                    .get::<RigidBody>(b.entity)
                    .is_some_and(|r| *r == RigidBody::Static);
                is_static && want.get(*id).is_none_or(|(a, _)| *a != b.aabb)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(b) = self.bodies.remove(&id) {
                self.app.get_mut().world_mut().despawn(b.entity);
            }
        }
        for (id, (aabb, layer)) in want {
            if !self.bodies.contains_key(&id) {
                self.spawn_box(id, aabb, RigidBody::Static, layer);
            }
        }
        self.refresh();
    }

    /// Динамическое тело-ящик: масса считается по плотности 1000 кг/м³ (Avian).
    pub fn spawn_dynamic(&mut self, id: ObjectId, aabb: Aabb, layer: u32) {
        self.spawn_box(id, aabb, RigidBody::Dynamic, layer);
        self.refresh();
    }

    /// Кинематическое тело (платформа, лифт): двигается заданной скоростью, но не откликается.
    pub fn spawn_kinematic(&mut self, id: ObjectId, aabb: Aabb, layer: u32) {
        self.spawn_box(id, aabb, RigidBody::Kinematic, layer);
        self.refresh();
    }

    pub fn set_linear_velocity(&mut self, id: ObjectId, v: Vec3) -> bool {
        let Some(b) = self.bodies.get(&id) else {
            return false;
        };
        self.app
            .get_mut()
            .world_mut()
            .entity_mut(b.entity)
            .insert(LinearVelocity(to_bevy(v)));
        true
    }

    /// Центр тела сейчас.
    pub fn position(&self, id: ObjectId) -> Option<Vec3> {
        let b = self.bodies.get(&id)?;
        let app = self.app.borrow();
        app.world()
            .get::<Transform>(b.entity)
            .map(|t| from_bevy(t.translation))
    }

    pub fn linear_velocity(&self, id: ObjectId) -> Option<Vec3> {
        let b = self.bodies.get(&id)?;
        let app = self.app.borrow();
        app.world()
            .get::<LinearVelocity>(b.entity)
            .map(|v| from_bevy(v.0))
    }

    /// Выполнить `steps` шагов физики.
    pub fn step(&mut self, steps: u32) {
        let app = self.app.get_mut();
        for _ in 0..steps {
            app.update();
        }
    }

    pub fn dt(&self) -> f32 {
        self.dt
    }

    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Обновить деревья коллайдеров, чтобы запросы видели только что добавленное.
    fn refresh(&mut self) {
        self.app.get_mut().update();
    }

    fn id_of(&self, app: &App, e: Entity) -> Option<ObjectId> {
        app.world().get::<Tagged>(e).map(|t| t.0)
    }

    fn current_aabb(&self, id: ObjectId) -> Option<Aabb> {
        let b = self.bodies.get(&id)?;
        let size = b.aabb.size();
        let c = self.position(id)?;
        Some(Aabb::new(c - size * 0.5, c + size * 0.5))
    }

    fn run_query<R>(&self, f: impl FnOnce(&AvianSpatialQuery) -> R) -> R {
        let mut app = self.app.borrow_mut();
        let mut state = bevy::ecs::system::SystemState::<AvianSpatialQuery>::new(app.world_mut());
        let q = state
            .get(app.world())
            .expect("параметр запроса Avian доступен");
        f(&q)
    }
}

fn filter(mask: u32) -> SpatialQueryFilter {
    SpatialQueryFilter::from_mask(LayerMask(mask))
}

impl SpatialQuery for AvianWorld {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<RayHit> {
        PhysicsQuery::raycast_masked(self, origin, dir, max_distance, LAYER_ALL)
    }

    fn overlap_aabb(&self, aabb: &Aabb) -> Vec<ObjectId> {
        PhysicsQuery::overlap_masked(self, aabb, LAYER_ALL)
    }
}

impl PhysicsQuery for AvianWorld {
    fn raycast_masked(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        layers: u32,
    ) -> Option<RayHit> {
        let dir = Dir3::new(to_bevy(dir)).ok()?;
        let hit = self
            .run_query(|q| q.cast_ray(to_bevy(origin), dir, max_distance, true, &filter(layers)))?;
        let app = self.app.borrow();
        let id = self.id_of(&app, hit.entity)?;
        let d: bevy::math::Vec3 = dir.into();
        Some(RayHit {
            id,
            distance: hit.distance,
            point: origin + from_bevy(d) * hit.distance,
            normal: from_bevy(hit.normal),
        })
    }

    fn overlap_masked(&self, shape: &Aabb, layers: u32) -> Vec<ObjectId> {
        let c = Self::cuboid(shape);
        let found = self.run_query(|q| {
            q.shape_intersections(&c, to_bevy(shape.center()), Quat::IDENTITY, &filter(layers))
        });
        let app = self.app.borrow();
        let mut ids: Vec<ObjectId> = found
            .into_iter()
            .filter_map(|e| self.id_of(&app, e))
            // Avian считает касание граней пересечением; движок — только пересечение с объёмом.
            .filter(|id| {
                self.current_aabb(*id)
                    .is_none_or(|b| astrocore_physics::aabb_overlap(shape, &b))
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    fn shapecast(&self, shape: &Aabb, motion: Vec3, layers: u32) -> Option<ShapeHit> {
        let len = motion.length();
        let dir = Dir3::new(to_bevy(motion)).ok()?;
        let c = Self::cuboid(shape);
        let config = ShapeCastConfig::from_max_distance(len);
        let hit = self.run_query(|q| {
            q.cast_shape(
                &c,
                to_bevy(shape.center()),
                Quat::IDENTITY,
                dir,
                &config,
                &filter(layers),
            )
        })?;
        let app = self.app.borrow();
        let id = self.id_of(&app, hit.entity)?;
        Some(ShapeHit {
            id,
            fraction: (hit.distance / len).clamp(0.0, 1.0),
            normal: from_bevy(hit.normal1),
            point: shape.center() + motion * (hit.distance / len),
        })
    }

    fn contacts(&self, shape: &Aabb, layers: u32) -> Vec<Contact> {
        let mut out: Vec<Contact> = self
            .overlap_masked(shape, layers)
            .into_iter()
            .filter_map(|id| {
                let other = self.current_aabb(id)?;
                penetration(shape, &other).map(|(normal, depth)| Contact { id, normal, depth })
            })
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }
}

fn penetration(a: &Aabb, b: &Aabb) -> Option<(Vec3, f32)> {
    if !astrocore_physics::aabb_overlap(a, b) {
        return None;
    }
    let mut best: Option<(Vec3, f32)> = None;
    for axis in 0..3 {
        let to_neg = a.max[axis] - b.min[axis];
        let to_pos = b.max[axis] - a.min[axis];
        let (depth, sign) = if to_neg < to_pos {
            (to_neg, -1.0)
        } else {
            (to_pos, 1.0)
        };
        if best.is_none_or(|(_, d)| depth < d) {
            let mut n = Vec3::ZERO;
            n[axis] = sign;
            best = Some((n, depth));
        }
    }
    best
}

#[cfg(test)]
mod tests;
