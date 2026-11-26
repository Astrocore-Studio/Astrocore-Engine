//! Сочленения тел (`Robust.Shared/Physics/Dynamics/Joints`, `SharedJointSystem` @ 1afd692).
//! Перенесено дистанционное сочленение (`DistanceJoint`): тела держатся на расстоянии между
//! наименьшей и наибольшей длиной, при нулевой жёсткости — как на верёвке. Сочленение
//! хранится в компоненте [`Joints`] обоих тел; двигаются им только свободные (`Dynamic`)
//! тела, как в Box2D.

use std::collections::BTreeMap;

use astrocore_ecs::{EventWorldExt, LifecycleWorldExt};
use astrocore_map::TransformWorldExt;
use astrocore_math::Vec2;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

/// Дистанционное сочленение (`DistanceJoint`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceJoint {
    /// Длина покоя.
    pub length: f32,
    pub min_length: f32,
    pub max_length: f32,
    /// Жёсткость пружины; ноль — жёсткие пределы.
    pub stiffness: f32,
    pub damping: f32,
}

/// Вид сочленения.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointKind {
    Distance(DistanceJoint),
}

/// Сочленение двух тел (`Joint`).
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    pub id: String,
    pub body_a: Entity,
    pub body_b: Entity,
    /// Точки крепления в осях тел.
    pub anchor_a: Vec2,
    pub anchor_b: Vec2,
    /// Сталкиваются ли соединённые тела друг с другом.
    pub collide_connected: bool,
    pub kind: JointKind,
}

impl Joint {
    /// Второе тело сочленения.
    pub fn other(&self, entity: Entity) -> Entity {
        if self.body_a == entity {
            self.body_b
        } else {
            self.body_a
        }
    }

    pub fn distance(&self) -> Option<&DistanceJoint> {
        match &self.kind {
            JointKind::Distance(joint) => Some(joint),
        }
    }

    pub fn distance_mut(&mut self) -> Option<&mut DistanceJoint> {
        match &mut self.kind {
            JointKind::Distance(joint) => Some(joint),
        }
    }
}

/// Сочленения тела (`JointComponent`). Только на сервере: клиент получает положения тел.
#[derive(Component, Debug, Clone, Default)]
pub struct Joints {
    pub joints: BTreeMap<String, Joint>,
}

/// Сочленение добавлено (`JointAddedEvent`). На обоих телах.
#[derive(Debug, Clone)]
pub struct JointAdded {
    pub joint: Joint,
    pub other: Entity,
}

/// Сочленение убрано (`JointRemovedEvent`). На обоих телах.
#[derive(Debug, Clone)]
pub struct JointRemoved {
    pub joint: Joint,
    pub other: Entity,
}

/// Сочленения на `World`.
pub trait JointWorldExt {
    /// Дистанционное сочленение (`CreateDistanceJoint`): длина — нынешнее расстояние между
    /// точками крепления, пределы равны ей. Сочленение с тем же именем заменяется.
    fn create_distance_joint(
        &mut self,
        body_a: Entity,
        body_b: Entity,
        anchor_a: Vec2,
        anchor_b: Vec2,
        id: &str,
    ) -> Option<Joint>;
    /// Сочленение тела по имени.
    fn joint(&self, entity: Entity, id: &str) -> Option<&Joint>;
    /// Изменить сочленение у обоих тел.
    fn edit_joint(&mut self, entity: Entity, id: &str, edit: impl FnOnce(&mut Joint));
    /// Убрать сочленение (`RemoveJoint`).
    fn remove_joint(&mut self, entity: Entity, id: &str) -> bool;
    /// Убрать все сочленения тела (`ClearJoints`).
    fn clear_joints(&mut self, entity: Entity);
}

fn anchor_world(world: &World, entity: Entity, anchor: Vec2) -> Option<Vec2> {
    let transform = world.world_transform(entity)?;
    Some(transform.position + transform.rotation.rotate(anchor))
}

impl JointWorldExt for World {
    fn create_distance_joint(
        &mut self,
        body_a: Entity,
        body_b: Entity,
        anchor_a: Vec2,
        anchor_b: Vec2,
        id: &str,
    ) -> Option<Joint> {
        if body_a == body_b {
            return None;
        }
        let length = anchor_world(self, body_a, anchor_a)?
            .distance(anchor_world(self, body_b, anchor_b)?)
            .max(crate::LINEAR_SLOP);
        self.remove_joint(body_a, id);
        let joint = Joint {
            id: id.to_owned(),
            body_a,
            body_b,
            anchor_a,
            anchor_b,
            collide_connected: true,
            kind: JointKind::Distance(DistanceJoint {
                length,
                min_length: length,
                max_length: length,
                stiffness: 0.0,
                damping: 0.0,
            }),
        };
        for entity in [body_a, body_b] {
            self.ensure_comp::<Joints>(entity);
            if let Some(mut joints) = self.get_mut::<Joints>(entity) {
                joints.joints.insert(id.to_owned(), joint.clone());
            }
        }
        for (entity, other) in [(body_a, body_b), (body_b, body_a)] {
            self.raise(
                entity,
                &mut JointAdded {
                    joint: joint.clone(),
                    other,
                },
            );
        }
        self.wake_bodies(&[body_a, body_b]);
        Some(joint)
    }

    fn joint(&self, entity: Entity, id: &str) -> Option<&Joint> {
        self.get::<Joints>(entity)?.joints.get(id)
    }

    fn edit_joint(&mut self, entity: Entity, id: &str, edit: impl FnOnce(&mut Joint)) {
        let Some(mut joint) = self.joint(entity, id).cloned() else {
            return;
        };
        edit(&mut joint);
        for body in [joint.body_a, joint.body_b] {
            if let Some(mut joints) = self.get_mut::<Joints>(body) {
                joints.joints.insert(id.to_owned(), joint.clone());
            }
        }
    }

    fn remove_joint(&mut self, entity: Entity, id: &str) -> bool {
        let Some(joint) = self.joint(entity, id).cloned() else {
            return false;
        };
        for body in [joint.body_a, joint.body_b] {
            if let Some(mut joints) = self.get_mut::<Joints>(body) {
                joints.joints.remove(id);
            }
        }
        for (body, other) in [(joint.body_a, joint.body_b), (joint.body_b, joint.body_a)] {
            if self.is_alive(body) {
                self.raise(
                    body,
                    &mut JointRemoved {
                        joint: joint.clone(),
                        other,
                    },
                );
            }
        }
        self.wake_bodies(&[joint.body_a, joint.body_b]);
        true
    }

    fn clear_joints(&mut self, entity: Entity) {
        let ids: Vec<String> = self
            .get::<Joints>(entity)
            .map(|joints| joints.joints.keys().cloned().collect())
            .unwrap_or_default();
        for id in ids {
            self.remove_joint(entity, &id);
        }
    }
}

trait WakeBodies {
    fn wake_bodies(&mut self, entities: &[Entity]);
}

impl WakeBodies for World {
    fn wake_bodies(&mut self, entities: &[Entity]) {
        for entity in entities {
            if self.is_alive(*entity) {
                crate::PhysicsWorldExt::wake_body(self, *entity);
            }
        }
    }
}

/// Сочленения, у которых хотя бы одно тело считается в этом шаге.
pub(crate) fn active_joints(world: &World, bodies: impl IntoIterator<Item = Entity>) -> Vec<Joint> {
    let mut seen = std::collections::BTreeSet::new();
    let mut found = Vec::new();
    for body in bodies {
        let Some(joints) = world.get::<Joints>(body) else {
            continue;
        };
        for joint in joints.joints.values() {
            if seen.insert((joint.body_a, joint.body_b, joint.id.clone())) {
                found.push(joint.clone());
            }
        }
    }
    found
}
