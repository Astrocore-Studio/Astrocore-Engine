//! Жизненный цикл сущностей и компонентов.
//!
//! Образец — `Robust.Shared/GameObjects/EntityManager.cs`, `EntityManager.Components.cs` и
//! `EntityManager.LifeCycle.cs` @ 1afd692. Сущность проходит стадии [`LifeStage`]; компоненты
//! получают события `ComponentAdd → ComponentInit → ComponentStartup → MapInit` и
//! `ComponentShutdown → ComponentRemove`.
//!
//! События жизненного цикла приходят только при работе через [`LifecycleWorldExt`]. Вставка
//! компонента напрямую через `bevy_ecs` событий не поднимает: так создаются заготовки сущностей
//! до инициализации.

use std::collections::HashSet;
use std::time::Duration;

use bevy_ecs::component::{Component, ComponentId};
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use smallvec::SmallVec;

use crate::event::EventWorldExt;
use crate::meta::{LifeStage, MetaData, Paused};
use crate::timing::GameTime;

/// Компонент добавлен к сущности.
#[derive(Debug, Clone, Copy)]
pub struct ComponentAdd;

/// Компонент инициализируется: все компоненты сущности уже добавлены.
#[derive(Debug, Clone, Copy)]
pub struct ComponentInit;

/// Компонент запускается: все компоненты сущности уже инициализированы.
#[derive(Debug, Clone, Copy)]
pub struct ComponentStartup;

/// Разовая настройка сущности на инициализированной карте: случайный выбор, начальное
/// содержимое, заряд. На карте, которую редактирует маппер, не приходит.
#[derive(Debug, Clone, Copy)]
pub struct MapInit;

/// Компонент останавливается: удаляется он сам или его сущность.
#[derive(Debug, Clone, Copy)]
pub struct ComponentShutdown {
    /// Стадия сущности: `Terminating`, если удаляется вся сущность.
    pub entity_stage: LifeStage,
}

/// Компонент удаляется с сущности.
#[derive(Debug, Clone, Copy)]
pub struct ComponentRemove;

/// Сущность будет удалена: поднимается на ней и на всех её потомках до удаления.
#[derive(Debug, Clone, Copy)]
pub struct EntityTerminating {
    pub entity: Entity,
}

/// Сущность удалена; идентификатор больше не действителен. Только широковещательно.
#[derive(Debug, Clone, Copy)]
pub struct EntityDeleted {
    pub entity: Entity,
}

/// Сущность встала на паузу.
#[derive(Debug, Clone, Copy)]
pub struct EntityPaused;

/// Сущность снята с паузы; таймеры сдвигаются на `paused_for`.
#[derive(Debug, Clone, Copy)]
pub struct EntityUnpaused {
    pub paused_for: Duration,
}

/// Сущности, ожидающие удаления в конце тика.
#[derive(Resource, Default)]
pub struct DeletionQueue {
    order: Vec<Entity>,
    queued: HashSet<Entity>,
}

type ComponentIds = SmallVec<[ComponentId; 32]>;

fn component_ids(world: &World, entity: Entity) -> ComponentIds {
    world
        .get_entity(entity)
        .map(|e| e.archetype().components().iter().copied().collect())
        .unwrap_or_default()
}

fn stage(world: &World, entity: Entity) -> Option<LifeStage> {
    world.get::<MetaData>(entity).map(|meta| meta.stage)
}

fn set_stage(world: &mut World, entity: Entity, stage: LifeStage) {
    if let Some(mut meta) = world.get_mut::<MetaData>(entity) {
        meta.stage = stage;
    }
}

/// Поднимает событие для каждого компонента сущности, пока не останется необработанных:
/// обработчик может добавить компонент, и тот получит событие в этом же проходе.
fn raise_for_components<E: 'static>(world: &mut World, entity: Entity, mut event: E) {
    let mut done: HashSet<ComponentId> = HashSet::new();
    loop {
        let pending: ComponentIds = component_ids(world, entity)
            .into_iter()
            .filter(|id| !done.contains(id))
            .collect();
        if pending.is_empty() {
            return;
        }
        for id in pending {
            done.insert(id);
            world.raise_component(entity, id, &mut event);
        }
    }
}

/// Жизненный цикл на `World`.
pub trait LifecycleWorldExt {
    /// Создаёт сущность с метаданными в стадии `Spawned`.
    fn spawn_uninitialized(&mut self) -> Entity;

    /// Добавляет или заменяет компонент и поднимает события его жизненного цикла до текущей
    /// стадии сущности. `false` — сущности нет или она удаляется.
    fn add_comp<T: Component>(&mut self, entity: Entity, component: T) -> bool;

    /// Добавляет компонент со значением по умолчанию, если его нет. `true` — добавлен.
    fn ensure_comp<T: Component + Default>(&mut self, entity: Entity) -> bool;

    /// Удаляет компонент с событиями `ComponentShutdown` и `ComponentRemove`.
    fn remove_comp<T: Component>(&mut self, entity: Entity) -> bool;

    /// То же по идентификатору компонента.
    fn remove_comp_id(&mut self, entity: Entity, component: ComponentId) -> bool;

    /// Поднимает события жизненного цикла для компонента, вставленного напрямую.
    fn notify_comp_added(&mut self, entity: Entity, component: ComponentId);

    /// `ComponentInit` всем компонентам; стадия `Initialized`.
    fn initialize_entity(&mut self, entity: Entity);

    /// `ComponentStartup` всем компонентам.
    fn start_entity(&mut self, entity: Entity);

    /// Инициализация, запуск и, если карта инициализирована, `MapInit`.
    fn initialize_and_start(&mut self, entity: Entity, map_init: bool);

    /// `MapInit`; стадия `MapInitialized`. Повторный вызов ничего не делает.
    fn run_map_init(&mut self, entity: Entity);

    /// Удаляет сущность и всех её потомков немедленно.
    fn delete_entity(&mut self, entity: Entity);

    /// Откладывает удаление до [`LifecycleWorldExt::flush_deletions`] в конце тика.
    fn queue_delete(&mut self, entity: Entity);

    /// Удаляет сущности из очереди.
    fn flush_deletions(&mut self);

    /// Сущность существует и не удаляется.
    fn is_alive(&self, entity: Entity) -> bool;

    /// Ставит сущность на паузу или снимает с неё.
    fn set_entity_paused(&mut self, entity: Entity, paused: bool);
}

fn flag_termination(world: &mut World, entity: Entity) {
    set_stage(world, entity, LifeStage::Terminating);
    world.raise_and_broadcast(entity, &mut EntityTerminating { entity });
    for child in children(world, entity) {
        if stage(world, child).is_none_or(|s| s < LifeStage::Terminating) {
            flag_termination(world, child);
        }
    }
}

fn children(world: &World, entity: Entity) -> SmallVec<[Entity; 8]> {
    world
        .get::<Children>(entity)
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default()
}

fn delete_flagged(world: &mut World, entity: Entity) {
    for child in children(world, entity) {
        delete_flagged(world, child);
    }
    let Some(meta) = world.get::<MetaData>(entity) else {
        world.despawn(entity);
        return;
    };
    if meta.started {
        raise_for_components(
            world,
            entity,
            ComponentShutdown {
                entity_stage: LifeStage::Terminating,
            },
        );
    }
    raise_for_components(world, entity, ComponentRemove);
    set_stage(world, entity, LifeStage::Deleted);
    world.despawn(entity);
    world.raise_broadcast(&mut EntityDeleted { entity });
}

impl LifecycleWorldExt for World {
    fn spawn_uninitialized(&mut self) -> Entity {
        let created = self
            .get_resource::<GameTime>()
            .map(|time| time.tick)
            .unwrap_or_default();
        self.spawn(MetaData {
            created,
            ..MetaData::default()
        })
        .id()
    }

    fn add_comp<T: Component>(&mut self, entity: Entity, component: T) -> bool {
        if stage(self, entity).is_some_and(|s| s >= LifeStage::Terminating) {
            return false;
        }
        let Ok(mut target) = self.get_entity_mut(entity) else {
            return false;
        };
        target.insert(component);
        let id = self.register_component::<T>();
        self.notify_comp_added(entity, id);
        true
    }

    fn ensure_comp<T: Component + Default>(&mut self, entity: Entity) -> bool {
        if self.get_entity(entity).is_ok_and(|e| e.contains::<T>()) {
            return false;
        }
        self.add_comp(entity, T::default())
    }

    fn remove_comp<T: Component>(&mut self, entity: Entity) -> bool {
        match self.component_id::<T>() {
            Some(id) => self.remove_comp_id(entity, id),
            None => false,
        }
    }

    fn remove_comp_id(&mut self, entity: Entity, component: ComponentId) -> bool {
        if !self
            .get_entity(entity)
            .is_ok_and(|e| e.contains_id(component))
        {
            return false;
        }
        let meta = self.get::<MetaData>(entity);
        if meta.is_some_and(|m| m.started) {
            let entity_stage = meta.map(|m| m.stage).unwrap_or_default();
            self.raise_component(entity, component, &mut ComponentShutdown { entity_stage });
        }
        self.raise_component(entity, component, &mut ComponentRemove);
        if let Ok(mut target) = self.get_entity_mut(entity) {
            target.remove_by_id(component);
        }
        true
    }

    fn notify_comp_added(&mut self, entity: Entity, component: ComponentId) {
        self.raise_component(entity, component, &mut ComponentAdd);
        let Some(meta) = self.get::<MetaData>(entity) else {
            return;
        };
        let (stage, started) = (meta.stage, meta.started);
        // Во время инициализации сущности компонент получит события в общем проходе.
        if stage >= LifeStage::Initialized {
            self.raise_component(entity, component, &mut ComponentInit);
        }
        if started {
            self.raise_component(entity, component, &mut ComponentStartup);
        }
        if stage == LifeStage::MapInitialized {
            self.raise_component(entity, component, &mut MapInit);
        }
    }

    fn initialize_entity(&mut self, entity: Entity) {
        if stage(self, entity) != Some(LifeStage::Spawned) {
            return;
        }
        set_stage(self, entity, LifeStage::Initializing);
        raise_for_components(self, entity, ComponentInit);
        if stage(self, entity) == Some(LifeStage::Initializing) {
            set_stage(self, entity, LifeStage::Initialized);
        }
    }

    fn start_entity(&mut self, entity: Entity) {
        let Some(meta) = self.get::<MetaData>(entity) else {
            return;
        };
        if meta.started || meta.stage != LifeStage::Initialized {
            return;
        }
        raise_for_components(self, entity, ComponentStartup);
        if let Some(mut meta) = self.get_mut::<MetaData>(entity) {
            meta.started = true;
        }
    }

    fn initialize_and_start(&mut self, entity: Entity, map_init: bool) {
        self.initialize_entity(entity);
        self.start_entity(entity);
        if map_init {
            self.run_map_init(entity);
        }
    }

    fn run_map_init(&mut self, entity: Entity) {
        if stage(self, entity) != Some(LifeStage::Initialized) {
            return;
        }
        set_stage(self, entity, LifeStage::MapInitialized);
        self.raise(entity, &mut MapInit);
    }

    fn delete_entity(&mut self, entity: Entity) {
        if self.get_entity(entity).is_err() {
            return;
        }
        if stage(self, entity).is_some_and(|s| s >= LifeStage::Terminating) {
            return;
        }
        flag_termination(self, entity);
        delete_flagged(self, entity);
    }

    fn queue_delete(&mut self, entity: Entity) {
        let mut queue = self.get_resource_or_insert_with(DeletionQueue::default);
        if queue.queued.insert(entity) {
            queue.order.push(entity);
        }
    }

    fn flush_deletions(&mut self) {
        // Удаление может поставить в очередь новые сущности: они удалятся в этом же вызове.
        loop {
            let Some(mut queue) = self.get_resource_mut::<DeletionQueue>() else {
                return;
            };
            if queue.order.is_empty() {
                return;
            }
            queue.queued.clear();
            let batch = std::mem::take(&mut queue.order);
            for entity in batch {
                self.delete_entity(entity);
            }
        }
    }

    fn is_alive(&self, entity: Entity) -> bool {
        self.get_entity(entity).is_ok()
            && stage(self, entity).is_none_or(|s| s < LifeStage::Terminating)
    }

    fn set_entity_paused(&mut self, entity: Entity, paused: bool) {
        let now = self
            .get_resource::<GameTime>()
            .map(GameTime::cur_time)
            .unwrap_or_default();
        let Some(mut meta) = self.get_mut::<MetaData>(entity) else {
            return;
        };
        if meta.paused() == paused {
            return;
        }
        if paused {
            meta.pause_time = Some(now);
            self.entity_mut(entity).insert(Paused);
            self.raise(entity, &mut EntityPaused);
        } else {
            let paused_for = now.saturating_sub(meta.pause_time.take().unwrap_or(now));
            self.entity_mut(entity).remove::<Paused>();
            self.raise(entity, &mut EntityUnpaused { paused_for });
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::hierarchy::ChildOf;

    use super::*;
    use crate::timing::GameTick;

    #[derive(Component, Default)]
    struct Lamp;
    #[derive(Component, Default)]
    struct Battery;
    #[derive(Resource, Default)]
    struct Log(Vec<String>);

    fn log(world: &mut World, line: impl Into<String>) {
        world.resource_mut::<Log>().0.push(line.into());
    }

    fn take_log(world: &mut World) -> Vec<String> {
        std::mem::take(&mut world.resource_mut::<Log>().0)
    }

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<Log>();
        world.subscribe::<Lamp, ComponentAdd>(|w, _, _| log(w, "lamp add"));
        world.subscribe::<Lamp, ComponentInit>(|w, _, _| log(w, "lamp init"));
        world.subscribe::<Lamp, ComponentStartup>(|w, _, _| log(w, "lamp startup"));
        world.subscribe::<Lamp, MapInit>(|w, _, _| log(w, "lamp mapinit"));
        world.subscribe::<Lamp, ComponentShutdown>(|w, _, ev: &mut ComponentShutdown| {
            log(w, format!("lamp shutdown {:?}", ev.entity_stage))
        });
        world.subscribe::<Lamp, ComponentRemove>(|w, _, _| log(w, "lamp remove"));
        world.subscribe::<Battery, ComponentInit>(|w, _, _| log(w, "battery init"));
        world.subscribe::<Battery, ComponentStartup>(|w, _, _| log(w, "battery startup"));
        world
    }

    #[test]
    fn full_lifecycle_in_robust_order() {
        let mut world = world();
        let entity = world.spawn_uninitialized();
        world.add_comp(entity, Lamp);
        assert_eq!(take_log(&mut world), ["lamp add"]);

        world.initialize_and_start(entity, true);
        assert_eq!(
            take_log(&mut world),
            ["lamp init", "lamp startup", "lamp mapinit"]
        );
        let meta = world.get::<MetaData>(entity).unwrap();
        assert_eq!(meta.stage, LifeStage::MapInitialized);
        assert!(meta.started);

        world.delete_entity(entity);
        assert_eq!(
            take_log(&mut world),
            ["lamp shutdown Terminating", "lamp remove"]
        );
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn component_added_to_running_entity_catches_up() {
        let mut world = world();
        let entity = world.spawn_uninitialized();
        world.initialize_and_start(entity, false);
        world.add_comp(entity, Lamp);
        assert_eq!(
            take_log(&mut world),
            ["lamp add", "lamp init", "lamp startup"]
        );
        world.run_map_init(entity);
        world.run_map_init(entity);
        assert_eq!(take_log(&mut world), ["lamp mapinit"]);

        assert!(world.remove_comp::<Lamp>(entity));
        assert_eq!(
            take_log(&mut world),
            ["lamp shutdown MapInitialized", "lamp remove"]
        );
        assert!(!world.remove_comp::<Lamp>(entity));
        assert!(world.ensure_comp::<Lamp>(entity));
        assert!(!world.ensure_comp::<Lamp>(entity));
    }

    #[test]
    fn component_added_during_init_is_initialized_in_same_pass() {
        let mut world = World::new();
        world.init_resource::<Log>();
        world.subscribe::<Lamp, ComponentInit>(|w, e, _| {
            log(w, "lamp init");
            w.add_comp(e, Battery);
        });
        world.subscribe::<Battery, ComponentInit>(|w, _, _| log(w, "battery init"));
        world.subscribe::<Battery, ComponentStartup>(|w, _, _| log(w, "battery startup"));
        let entity = world.spawn_uninitialized();
        world.entity_mut(entity).insert(Lamp);
        world.initialize_and_start(entity, false);
        assert_eq!(
            take_log(&mut world),
            ["lamp init", "battery init", "battery startup"]
        );
    }

    #[test]
    fn uninitialized_entity_is_removed_without_shutdown() {
        let mut world = world();
        let entity = world.spawn_uninitialized();
        world.add_comp(entity, Lamp);
        take_log(&mut world);
        world.delete_entity(entity);
        assert_eq!(take_log(&mut world), ["lamp remove"]);
    }

    #[test]
    fn deletion_terminates_parent_first_and_deletes_children_first() {
        let mut world = World::new();
        world.init_resource::<Log>();
        world.subscribe_broadcast::<EntityTerminating>(|w, ev| {
            let name = w.get::<MetaData>(ev.entity).unwrap().name.clone().unwrap();
            log(w, format!("terminating {name}"));
        });
        world.subscribe::<Lamp, ComponentRemove>(|w, e, _| {
            let name = w.get::<MetaData>(e).unwrap().name.clone().unwrap();
            log(w, format!("remove {name}"));
        });
        world.subscribe_broadcast::<EntityDeleted>(|w, _| log(w, "deleted"));
        let mut spawn = |name: &str, parent: Option<Entity>| {
            let entity = world.spawn_uninitialized();
            world.get_mut::<MetaData>(entity).unwrap().name = Some(name.into());
            world.entity_mut(entity).insert(Lamp);
            if let Some(parent) = parent {
                world.entity_mut(entity).insert(ChildOf(parent));
            }
            world.initialize_and_start(entity, false);
            entity
        };
        let station = spawn("station", None);
        let locker = spawn("locker", Some(station));
        let crowbar = spawn("crowbar", Some(locker));

        world.queue_delete(station);
        world.queue_delete(station);
        assert!(world.is_alive(crowbar));
        world.flush_deletions();
        assert_eq!(
            take_log(&mut world),
            [
                "terminating station",
                "terminating locker",
                "terminating crowbar",
                "remove crowbar",
                "deleted",
                "remove locker",
                "deleted",
                "remove station",
                "deleted",
            ]
        );
        assert!(!world.is_alive(station) && !world.is_alive(locker) && !world.is_alive(crowbar));
    }

    #[test]
    fn deleting_inside_termination_handler_does_not_recurse() {
        let mut world = World::new();
        world.subscribe::<Lamp, EntityTerminating>(|w, e, _| w.delete_entity(e));
        world.subscribe::<Lamp, ComponentRemove>(|w, e, _| {
            assert!(!w.add_comp(e, Battery));
            w.queue_delete(e);
        });
        let entity = world.spawn_uninitialized();
        world.add_comp(entity, Lamp);
        world.delete_entity(entity);
        world.flush_deletions();
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn pause_marks_entity_and_reports_duration() {
        let mut world = World::new();
        world.init_resource::<Log>();
        world.insert_resource(GameTime::default());
        world.subscribe::<Lamp, EntityPaused>(|w, _, _| log(w, "paused"));
        world.subscribe::<Lamp, EntityUnpaused>(|w, _, ev: &mut EntityUnpaused| {
            log(w, format!("unpaused {}", ev.paused_for.as_secs()))
        });
        let entity = world.spawn_uninitialized();
        world.add_comp(entity, Lamp);
        world.set_entity_paused(entity, true);
        world.set_entity_paused(entity, true);
        assert!(world.entity(entity).contains::<Paused>());
        world.resource_mut::<GameTime>().tick = GameTick(1 + 30 * 5);
        world.set_entity_paused(entity, false);
        assert!(!world.entity(entity).contains::<Paused>());
        assert_eq!(take_log(&mut world), ["paused", "unpaused 5"]);
    }
}
