//! Профили Bevy-плагинов Astrocore (ADR-0001, раздел 4 дизайн-документа).
//!
//! Крейт связывает канонический мир ([`astrocore_world::World`]) с Bevy ECS и
//! задаёт расписание симуляции. Он **не** тянет оконную систему, рендер и звук:
//! это гарантируется набором фич Bevy (`default-features = false`) и проверяется
//! в CI скриптом `scripts/check-headless.sh`.
//!
//! Стадии фиксированного шага (раздел 6): Input → Commands → Simulation →
//! WorldChanges → Replication → Presentation. Порядок задаётся через
//! [`SimSet`] и проверяется тестом.

use std::collections::{HashMap, HashSet};

use astrocore_core::ObjectId;
use astrocore_world::{DirtyRegion, World, WorldCommand, WorldError, WorldOp};
use bevy::prelude::*;

/// Стадии серверного/клиентского фиксированного шага.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimSet {
    Input,
    Commands,
    Simulation,
    WorldChanges,
    Replication,
    Presentation,
}

/// Канонический мир как ресурс ECS. Единственный источник истины о структуре.
#[derive(Resource)]
pub struct CanonicalWorld(pub World);

/// Очередь команд изменения мира; обрабатывается на стадии `Commands`.
#[derive(Resource, Default)]
pub struct CommandQueue(pub Vec<WorldCommand>);

/// Подтверждённые за тик операции (для журнала и репликации). Потребитель обязан
/// очищать буфер на стадии `Replication`.
#[derive(Resource, Default)]
pub struct OpOutbox(pub Vec<WorldOp>);

/// Отклонённые команды за тик (для обратной связи автору команды).
#[derive(Resource, Default)]
pub struct RejectedCommands(pub Vec<(WorldCommand, WorldError)>);

/// Регионы, которые нужно пересобрать производным системам.
#[derive(Resource, Default)]
pub struct DirtyInbox(pub Vec<DirtyRegion>);

/// Связь постоянного `ObjectId` с runtime-`Entity`. `Entity` никогда не
/// сериализуется (принцип 6 раздела 3).
#[derive(Resource, Default)]
pub struct ObjectIndex {
    entities: HashMap<ObjectId, Entity>,
}

impl ObjectIndex {
    pub fn entity(&self, id: ObjectId) -> Option<Entity> {
        self.entities.get(&id).copied()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

/// Постоянный идентификатор объекта на его runtime-сущности.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentId(pub ObjectId);

/// Ревизия мира, с которой синхронизированы сущности.
#[derive(Resource, Default)]
struct SyncedRevision(u64);

/// Плагин серверной симуляции: мир, очередь команд, расписание, синхронизация сущностей.
pub struct SimulationPlugin {
    pub world: World,
    pub tick_hz: f64,
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CanonicalWorld(self.world.clone()))
            .insert_resource(Time::<Fixed>::from_hz(self.tick_hz))
            .init_resource::<CommandQueue>()
            .init_resource::<OpOutbox>()
            .init_resource::<RejectedCommands>()
            .init_resource::<DirtyInbox>()
            .init_resource::<ObjectIndex>()
            .init_resource::<SyncedRevision>()
            .configure_sets(
                FixedUpdate,
                (
                    SimSet::Input,
                    SimSet::Commands,
                    SimSet::Simulation,
                    SimSet::WorldChanges,
                    SimSet::Replication,
                    SimSet::Presentation,
                )
                    .chain(),
            )
            .add_systems(FixedUpdate, apply_commands.in_set(SimSet::Commands))
            .add_systems(
                FixedUpdate,
                (collect_dirty, sync_entities)
                    .chain()
                    .in_set(SimSet::WorldChanges),
            );
    }
}

/// Приложение без окна, рендера и звука: `MinimalPlugins` + симуляция.
pub fn headless_app(world: World, tick_hz: f64) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(SimulationPlugin { world, tick_hz });
    app
}

fn apply_commands(
    mut world: ResMut<CanonicalWorld>,
    mut queue: ResMut<CommandQueue>,
    mut outbox: ResMut<OpOutbox>,
    mut rejected: ResMut<RejectedCommands>,
) {
    for command in std::mem::take(&mut queue.0) {
        match world.0.apply(command.clone()) {
            Ok(op) => outbox.0.push(op),
            Err(e) => rejected.0.push((command, e)),
        }
    }
}

fn collect_dirty(mut world: ResMut<CanonicalWorld>, mut inbox: ResMut<DirtyInbox>) {
    if world.0.has_dirty() {
        inbox.0.extend(world.0.take_dirty());
    }
}

/// Привести набор сущностей в соответствие с каноническим миром.
fn sync_entities(
    mut commands: Commands,
    world: Res<CanonicalWorld>,
    mut index: ResMut<ObjectIndex>,
    mut synced: ResMut<SyncedRevision>,
) {
    let revision = world.0.revision().0;
    if revision == synced.0 && (revision != 0 || !index.is_empty()) {
        return;
    }
    let present: HashSet<ObjectId> = world.0.elements().map(|e| e.id).collect();
    index.entities.retain(|id, entity| {
        let keep = present.contains(id);
        if !keep {
            commands.entity(*entity).despawn();
        }
        keep
    });
    for id in present {
        index
            .entities
            .entry(id)
            .or_insert_with(|| commands.spawn(PersistentId(id)).id());
    }
    synced.0 = revision;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use astrocore_core::PrototypeId;
    use astrocore_math::GridEdge;
    use astrocore_world::{
        FloorDef, FloorId, Placement, StructureElement, StructureKind, WorldConfig,
    };
    use bevy::time::TimeUpdateStrategy;

    use super::*;

    fn base_world() -> World {
        let mut w = World::new("t", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 0.0,
            height: 3.0,
        }))
        .unwrap();
        w.take_dirty();
        w
    }

    fn wall(id: u128, x: i32) -> WorldCommand {
        WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind: StructureKind::Wall,
            placement: Placement::Edge(GridEdge::along_x(x, 0)),
            floor: FloorId(0),
            material: PrototypeId::new("t:m").unwrap(),
            thickness: 0.2,
        })
    }

    /// Приложение, где каждый `update()` продвигает время ровно на один тик 20 Гц.
    fn app() -> App {
        let mut app = headless_app(base_world(), 20.0);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
        app.update(); // инициализация часов
        app
    }

    #[test]
    fn commands_apply_on_fixed_tick_and_spawn_entities() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<CommandQueue>()
            .0
            .push(wall(1, 0));
        app.update();
        let world = app.world();
        assert_eq!(world.resource::<CanonicalWorld>().0.element_count(), 1);
        assert_eq!(world.resource::<OpOutbox>().0.len(), 1);
        let entity = world
            .resource::<ObjectIndex>()
            .entity(ObjectId::from_u128(1))
            .expect("сущность создана");
        assert_eq!(
            world.get::<PersistentId>(entity),
            Some(&PersistentId(ObjectId::from_u128(1)))
        );
        assert!(
            !world.resource::<DirtyInbox>().0.is_empty(),
            "dirty-регионы переданы потребителям"
        );
    }

    #[test]
    fn removed_elements_despawn_their_entities() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<CommandQueue>()
            .0
            .push(wall(1, 0));
        app.update();
        let entity = app
            .world()
            .resource::<ObjectIndex>()
            .entity(ObjectId::from_u128(1))
            .unwrap();
        app.world_mut()
            .resource_mut::<CommandQueue>()
            .0
            .push(WorldCommand::RemoveElement {
                id: ObjectId::from_u128(1),
            });
        app.update();
        assert!(app.world().resource::<ObjectIndex>().is_empty());
        assert!(
            app.world().get_entity(entity).is_err(),
            "runtime-сущность удалена"
        );
    }

    #[test]
    fn invalid_commands_are_reported_not_applied() {
        let mut app = app();
        {
            let mut q = app.world_mut().resource_mut::<CommandQueue>();
            q.0.push(wall(1, 0));
            q.0.push(wall(2, 0)); // то же место
        }
        app.update();
        let w = app.world();
        assert_eq!(w.resource::<CanonicalWorld>().0.element_count(), 1);
        assert_eq!(w.resource::<RejectedCommands>().0.len(), 1);
        assert!(matches!(
            w.resource::<RejectedCommands>().0[0].1,
            WorldError::SlotOccupied { .. }
        ));
    }

    #[test]
    fn stages_run_in_declared_order() {
        #[derive(Resource, Default)]
        struct Trace(Vec<&'static str>);
        let mut app = app();
        app.init_resource::<Trace>().add_systems(
            FixedUpdate,
            (
                (|mut t: ResMut<Trace>| t.0.push("presentation")).in_set(SimSet::Presentation),
                (|mut t: ResMut<Trace>| t.0.push("input")).in_set(SimSet::Input),
                (|mut t: ResMut<Trace>| t.0.push("replication")).in_set(SimSet::Replication),
                (|mut t: ResMut<Trace>| t.0.push("simulation")).in_set(SimSet::Simulation),
                (|mut t: ResMut<Trace>| t.0.push("world_changes")).in_set(SimSet::WorldChanges),
                (|mut t: ResMut<Trace>| t.0.push("commands")).in_set(SimSet::Commands),
            ),
        );
        app.update();
        assert_eq!(
            app.world().resource::<Trace>().0,
            [
                "input",
                "commands",
                "simulation",
                "world_changes",
                "replication",
                "presentation"
            ]
        );
    }

    #[test]
    fn fixed_step_is_independent_of_frame_rate() {
        // Два кадра по 25 мс = один тик 20 Гц.
        let mut app = headless_app(base_world(), 20.0);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            25,
        )));
        #[derive(Resource, Default)]
        struct Ticks(u32);
        app.init_resource::<Ticks>()
            .add_systems(FixedUpdate, |mut t: ResMut<Ticks>| t.0 += 1);
        app.update();
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<Ticks>().0,
            4,
            "8 кадров по 25 мс = 4 тика по 50 мс"
        );
    }
}
