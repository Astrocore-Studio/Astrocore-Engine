//! Отметки изменений: какой сущности есть что отправить клиентам.

use std::any::TypeId;
use std::collections::HashSet;

use astrocore_ecs::{GameTime, GameWorldExt, Sync};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::RemovedComponents;
use bevy_ecs::query::Changed;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy_ecs::system::{Query, Res};
use bevy_ecs::world::World;

use crate::ids::NetSync;

/// Порядок систем этапа `Sync`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetSet {
    /// Новым сущностям выдаются сетевые номера.
    Assign,
    /// Отмечаются сущности с изменившимися компонентами.
    Mark,
    /// Собираются и отправляются состояния.
    Send,
}

/// Типы, для которых уже добавлена система отметок.
#[derive(Resource, Default)]
struct Tracked(HashSet<TypeId>);

fn mark_changed<T: Component>(
    time: Res<GameTime>,
    changed: Query<Entity, Changed<T>>,
    mut removed: RemovedComponents<T>,
    mut sync: Query<&mut NetSync>,
) {
    for entity in &changed {
        if let Ok(mut sync) = sync.get_mut(entity) {
            sync.last_modified = time.tick;
        }
    }
    for entity in removed.read() {
        if let Ok(mut sync) = sync.get_mut(entity) {
            sync.last_modified = time.tick;
            sync.last_removed = time.tick;
        }
    }
}

/// Добавляет систему, отмечающую изменения компонента `T`.
pub(crate) fn track_changes<T: Component>(world: &mut World) {
    world.init_game();
    if !world
        .get_resource_or_insert_with(Tracked::default)
        .0
        .insert(TypeId::of::<T>())
    {
        return;
    }
    world.add_game_systems(Sync, mark_changed::<T>.in_set(NetSet::Mark));
}
