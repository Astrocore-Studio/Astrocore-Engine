//! Физика Astrocore по образцу `Robust.Shared/Physics`: тела, фикстуры, слои столкновений,
//! события касаний и шаг симуляции для вида сверху.
//!
//! Что перенесено упрощённо: столкновения решаются расталкиванием и гашением сближающей
//! скорости, без трения между телами, вращения от ударов и сочленений. Тела сталкиваются
//! внутри одного грида или космоса карты и одного этажа.

mod body;
mod shapes;
mod world;

use astrocore_ecs::{
    ComponentShutdown, ComponentStartup, EntityTerminating, EventWorldExt, GamePlugin,
    GameWorldExt, PostUpdate, RegistryWorldExt,
};
use astrocore_map::{AnchorStateChanged, MoveEvent};
use astrocore_net::NetRegistryWorldExt;
use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::world::World;

pub use body::{
    BodyStatus, BodyType, CollisionGroups, Fixture, Fixtures, Physics, register_collision_group,
};
pub use shapes::{Manifold, Pose, Shape, WorldShape, collide};
pub use world::{
    Broadphase, EndCollide, LINEAR_SLOP, PhysicsWorldExt, PreventCollide, SLEEP_SPEED,
    StartCollide, TIME_TO_SLEEP, step_physics,
};

/// Подключает физику: компоненты `Physics` и `Fixtures`, сетку поиска столкновений и шаг
/// симуляции на этапе `PostUpdate`. Подключается после сетевого плагина.
pub struct PhysicsPlugin {
    /// Считать ли шаг симуляции каждый такт. Клиент не считает: положения тел приходят
    /// с сервера, а своё тело клиент предсказывает отдельно.
    pub simulate: bool,
}

impl PhysicsPlugin {
    /// Физика сервера: шаг симуляции каждый такт.
    pub const SERVER: PhysicsPlugin = PhysicsPlugin { simulate: true };
    /// Физика клиента: только компоненты и сетка поиска.
    pub const CLIENT: PhysicsPlugin = PhysicsPlugin { simulate: false };
}

impl GamePlugin for PhysicsPlugin {
    fn build(&self, world: &mut World) {
        world.register_data_component::<Physics>();
        world.register_data_component::<Fixtures>();
        world.register_net_serde::<Physics>();
        world.register_net_serde::<Fixtures>();
        world.init_resource::<Broadphase>();

        world.subscribe::<Fixtures, ComponentStartup>(|world, entity, _| {
            let mass = world.get::<Fixtures>(entity).map_or(0.0, Fixtures::mass);
            if let Some(mut body) = world.get_mut::<Physics>(entity) {
                body.bypass_change_detection().mass = mass;
            }
            world::update_proxy(world, entity);
        });
        world.subscribe::<Physics, ComponentStartup>(|world, entity, _| {
            let mass = world.get::<Fixtures>(entity).map_or(0.0, Fixtures::mass);
            if let Some(mut body) = world.get_mut::<Physics>(entity) {
                body.bypass_change_detection().mass = mass;
            }
            world::update_proxy(world, entity);
        });
        world.subscribe::<Fixtures, MoveEvent>(|world, entity, _| {
            world::update_proxy(world, entity);
        });
        world.subscribe::<Fixtures, ComponentShutdown>(|world, entity, _| {
            remove_from_broadphase(world, entity);
        });
        world.subscribe::<Fixtures, EntityTerminating>(|world, entity, _| {
            remove_from_broadphase(world, entity);
        });
        // Привязанное к клетке тело неподвижно; отвязанное снова подчиняется столкновениям.
        world.subscribe::<Physics, AnchorStateChanged>(
            |world, entity, event: &mut AnchorStateChanged| {
                if event.detaching {
                    return;
                }
                let body_type = if event.anchored {
                    BodyType::Static
                } else {
                    BodyType::Dynamic
                };
                world.set_body_type(entity, body_type);
            },
        );
        if self.simulate {
            world.add_game_systems(PostUpdate, world::physics_system);
        }
    }
}

/// Убирает тело из сетки поиска: его касания заканчиваются.
fn remove_from_broadphase(world: &mut World, entity: Entity) {
    let Some(mut body) = world.get_mut::<Physics>(entity) else {
        return;
    };
    let collided = std::mem::replace(&mut body.bypass_change_detection().can_collide, false);
    world::update_proxy(world, entity);
    if let Some(mut body) = world.get_mut::<Physics>(entity) {
        body.bypass_change_detection().can_collide = collided;
    }
}

use bevy_ecs::entity::Entity;

#[cfg(test)]
mod tests;
