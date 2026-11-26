//! Переход между гридами: сущность, вышедшая за край своего грида, становится дочерней
//! грида под ней, а без грида — дочерней карты. Образец — `SharedGridTraversalSystem`
//! Robust @ 1afd692.

use astrocore_ecs::{EventWorldExt, MetaData};
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use crate::grid::MapGrid;
use crate::map::{Map, MapWorldExt};
use crate::transform::{MoveEvent, Transform, TransformWorldExt};

/// Настройка перехода между гридами. Ресурс `World`.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct GridTraversal {
    /// Сколько раз переход приостановлен. Клиент приостанавливает его, пока применяет
    /// состояние сервера: родителя сущности там задаёт сервер.
    pub suspended: u32,
}

fn traverse(world: &mut World, entity: Entity, event: &mut MoveEvent) {
    if world.resource::<GridTraversal>().suspended > 0
        || (event.old_position == event.new_position && !event.parent_changed())
    {
        return;
    }
    let Some(transform) = world.get::<Transform>(entity) else {
        return;
    };
    let (Some(map), Some(parent)) = (transform.map(), world.parent(entity)) else {
        return;
    };
    // Переходят сущности верхнего уровня: привязанные к клетке, лежащие в контейнерах и
    // сами гриды остаются на месте. Сущность из файла карты сохраняет своего родителя, пока
    // не инициализирована.
    let top_level = parent == map || world.get::<MapGrid>(parent).is_some();
    if !top_level
        || transform.anchored()
        || world.get::<MapGrid>(entity).is_some()
        || world.get::<Map>(entity).is_some()
        || world
            .get::<MetaData>(entity)
            .is_none_or(|meta| !meta.initialized())
    {
        return;
    }
    let Some(coordinates) = world.map_coordinates(entity) else {
        return;
    };
    let target = world.find_grid_at(coordinates).unwrap_or(map);
    if target != parent {
        // Смена родителя сохраняет положение на карте и поднимает `MoveEvent` заново:
        // там цель совпадёт с родителем.
        let _ = world.set_parent(entity, target);
    }
}

pub(crate) fn register(world: &mut World) {
    world.init_resource::<GridTraversal>();
    world.subscribe::<Transform, MoveEvent>(traverse);
}
