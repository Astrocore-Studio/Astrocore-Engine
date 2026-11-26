//! Игровой мир и такт симуляции.
//!
//! Такт: `PreUpdate → Update → PostUpdate`, затем отложенные удаления и переход к следующему
//! тику. Системы внутри этапа расписание Bevy исполняет параллельно, если их данные не
//! пересекаются; системы с `&mut World` исполняются по одной.

use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, Schedules};
use bevy_ecs::system::ScheduleSystem;
use bevy_ecs::world::World;
use bevy_tasks::{ComputeTaskPool, TaskPool};

use crate::event::EventBus;
use crate::lifecycle::{DeletionQueue, LifecycleWorldExt};
use crate::random::GameRng;
use crate::registry::ComponentRegistry;
use crate::timing::GameTime;

/// Этап перед обновлением: ввод игроков, сетевые сообщения.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreUpdate;

/// Основной этап: игровые системы.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Update;

/// Этап после обновления: физика, итоги тика, подготовка сетевых состояний.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PostUpdate;

/// Часть движка или игры, которая добавляет в мир компоненты, подписки и системы
/// (`EntitySystem.Initialize` в Robust).
pub trait GamePlugin {
    fn build(&self, world: &mut World);
}

impl<F: Fn(&mut World)> GamePlugin for F {
    fn build(&self, world: &mut World) {
        self(world);
    }
}

/// Такт и расписания на `World`.
pub trait GameWorldExt {
    /// Готовит мир к работе: ресурсы ядра и расписания этапов. Повторный вызов безвреден.
    fn init_game(&mut self);

    /// Подключает часть движка или игры.
    fn add_plugin(&mut self, plugin: impl GamePlugin);

    /// Добавляет системы в этап такта.
    fn add_game_systems<M>(
        &mut self,
        stage: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    );

    /// Один такт симуляции.
    fn run_tick(&mut self);
}

impl GameWorldExt for World {
    fn init_game(&mut self) {
        // Параллельному исполнителю расписаний нужен пул потоков.
        ComputeTaskPool::get_or_init(TaskPool::default);
        self.init_resource::<EventBus>();
        self.init_resource::<ComponentRegistry>();
        self.init_resource::<GameTime>();
        self.init_resource::<GameRng>();
        self.init_resource::<DeletionQueue>();
        self.init_resource::<Schedules>();
        let mut schedules = self.resource_mut::<Schedules>();
        if !schedules.contains(PreUpdate) {
            schedules.insert(Schedule::new(PreUpdate));
            schedules.insert(Schedule::new(Update));
            schedules.insert(Schedule::new(PostUpdate));
        }
    }

    fn add_plugin(&mut self, plugin: impl GamePlugin) {
        self.init_game();
        plugin.build(self);
    }

    fn add_game_systems<M>(
        &mut self,
        stage: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) {
        self.init_game();
        self.resource_mut::<Schedules>().add_systems(stage, systems);
    }

    fn run_tick(&mut self) {
        if self.resource::<GameTime>().paused {
            return;
        }
        self.run_schedule(PreUpdate);
        self.run_schedule(Update);
        self.run_schedule(PostUpdate);
        self.flush_deletions();
        let mut time = self.resource_mut::<GameTime>();
        time.tick = time.tick.next();
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::component::Component;
    use bevy_ecs::system::{Query, Res};

    use super::*;
    use crate::meta::{Active, MetaData};
    use crate::timing::GameTick;

    #[derive(Component, Default)]
    struct Charge(f32);

    fn charge(time: Res<GameTime>, mut cells: Query<&mut Charge, Active>) {
        for mut cell in &mut cells {
            cell.0 += time.frame_time();
        }
    }

    fn expire(world: &mut World) {
        let spent: Vec<_> = world
            .query::<(bevy_ecs::entity::Entity, &Charge)>()
            .iter(world)
            .filter(|(_, c)| c.0 >= 1.0)
            .map(|(e, _)| e)
            .collect();
        for entity in spent {
            world.queue_delete(entity);
        }
    }

    #[test]
    fn tick_runs_stages_skips_paused_and_flushes_deletions() {
        let mut world = World::new();
        world.add_plugin(|world: &mut World| {
            world.add_game_systems(Update, charge);
            world.add_game_systems(PostUpdate, expire);
        });
        let running = world.spawn_uninitialized();
        world.add_comp(running, Charge::default());
        let frozen = world.spawn_uninitialized();
        world.add_comp(frozen, Charge::default());
        world.set_entity_paused(frozen, true);

        for _ in 0..29 {
            world.run_tick();
        }
        assert!(world.is_alive(running));
        for _ in 0..2 {
            world.run_tick();
        }
        assert!(!world.is_alive(running));
        assert_eq!(world.get::<Charge>(frozen).unwrap().0, 0.0);
        assert_eq!(world.resource::<GameTime>().tick, GameTick(32));
        assert_eq!(world.get::<MetaData>(frozen).unwrap().created, GameTick(1));

        world.resource_mut::<GameTime>().paused = true;
        world.run_tick();
        assert_eq!(world.resource::<GameTime>().tick, GameTick(32));
    }
}
