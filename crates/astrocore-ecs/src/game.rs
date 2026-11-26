//! Игровой мир и такт симуляции.
//!
//! Такт: `PreUpdate → Update → PostUpdate`, отложенные удаления, `Sync` и переход к следующему
//! тику. Системы внутри этапа расписание Bevy исполняет параллельно, если их данные не
//! пересекаются; системы с `&mut World` исполняются по одной.

use std::collections::VecDeque;

use bevy_ecs::change_detection::{ComponentTicks, Tick};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, Schedules};
use bevy_ecs::system::ScheduleSystem;
use bevy_ecs::world::World;
use bevy_tasks::{ComputeTaskPool, TaskPool};

use crate::event::EventBus;
use crate::lifecycle::{DeletionQueue, LifecycleWorldExt};
use crate::random::GameRng;
use crate::registry::ComponentRegistry;
use crate::timing::{GameTick, GameTime};

/// Этап перед обновлением: ввод игроков, сетевые сообщения.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreUpdate;

/// Основной этап: игровые системы.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Update;

/// Этап после обновления: физика, итоги тика, подготовка сетевых состояний.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PostUpdate;

/// Этап после удалений: мир за тик больше не меняется, собираются сетевые состояния.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sync;

/// Граница тика в счётчике изменений Bevy: по ней определяется, менялся ли компонент после
/// заданного игрового тика. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct ChangeHistory {
    /// Игровой тик → значение счётчика изменений на его конец; по возрастанию тиков.
    boundaries: VecDeque<(GameTick, Tick)>,
}

/// Сколько тиков хранится в истории границ (около минуты при 30 Гц).
const HISTORY_TICKS: usize = 2048;

impl ChangeHistory {
    fn record(&mut self, tick: GameTick, boundary: Tick) {
        if self.boundaries.len() == HISTORY_TICKS {
            self.boundaries.pop_front();
        }
        self.boundaries.push_back((tick, boundary));
    }

    /// Значение счётчика изменений на конец игрового тика, если он ещё в истории.
    pub fn boundary(&self, tick: GameTick) -> Option<Tick> {
        let (first, _) = *self.boundaries.front()?;
        let index = tick.0.checked_sub(first.0)? as usize;
        self.boundaries
            .get(index)
            .filter(|(recorded, _)| *recorded == tick)
            .map(|(_, boundary)| *boundary)
    }

    /// Компонент менялся после конца тика `since`. Для тика вне истории ответ — «да»:
    /// лишняя отправка безвредна, пропущенная — нет.
    pub fn changed_since(&self, ticks: ComponentTicks, since: GameTick, now: Tick) -> bool {
        match self.boundary(since) {
            Some(boundary) => ticks.changed.is_newer_than(boundary, now),
            None => true,
        }
    }
}

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
        self.init_resource::<ChangeHistory>();
        self.init_resource::<Schedules>();
        let mut schedules = self.resource_mut::<Schedules>();
        if !schedules.contains(PreUpdate) {
            schedules.insert(Schedule::new(PreUpdate));
            schedules.insert(Schedule::new(Update));
            schedules.insert(Schedule::new(PostUpdate));
            schedules.insert(Schedule::new(Sync));
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
        self.run_schedule(Sync);
        // Изменения следующего тика получат отметку строго больше границы этого.
        let tick = self.resource::<GameTime>().tick;
        let boundary = self.change_tick();
        self.resource_mut::<ChangeHistory>().record(tick, boundary);
        self.increment_change_tick();
        self.clear_trackers();
        self.resource_mut::<GameTime>().tick = tick.next();
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

#[cfg(test)]
mod change_tests {
    use bevy_ecs::component::Component;

    use super::*;
    use crate::timing::GameTick;

    #[derive(Component)]
    struct Health(u32);

    #[test]
    fn change_history_tells_ticks_apart() {
        let mut world = World::new();
        world.init_game();
        let entity = world.spawn(Health(10)).id();
        let changed_since = |world: &World, since: u32| {
            let ticks = world.entity(entity).get_change_ticks::<Health>().unwrap();
            world.resource::<ChangeHistory>().changed_since(
                ticks,
                GameTick(since),
                world.read_change_tick(),
            )
        };
        world.run_tick(); // тик 1: компонент добавлен
        world.run_tick(); // тик 2: без изменений
        world.get_mut::<Health>(entity).unwrap().0 = 5; // начало тика 3
        world.run_tick();
        world.run_tick(); // тик 4
        assert!(changed_since(&world, 1));
        assert!(changed_since(&world, 2));
        assert!(!changed_since(&world, 3));
        assert!(!changed_since(&world, 4));
        // Тика 0 в истории нет: считается изменённым.
        assert!(changed_since(&world, 0));
        assert!(
            world
                .resource::<ChangeHistory>()
                .boundary(GameTick(9))
                .is_none()
        );
    }
}
