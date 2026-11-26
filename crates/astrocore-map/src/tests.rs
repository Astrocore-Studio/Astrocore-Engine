use astrocore_data::DataFormat;
use astrocore_ecs::{
    ComponentRegistry, EventWorldExt, GameWorldExt, LifeStage, LifecycleWorldExt, MapInit,
    MetaData, Paused,
};
use astrocore_math::{Angle, Vec2};
use astrocore_prototypes::{LoadReport, Prototypes};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use super::*;

const FLOOR: Tile = Tile::new(1);

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
    world.add_plugin(MapPlugin);
    world.init_resource::<Log>();
    world
}

/// Карта с гридом 4×4 на этаже 0 и одним тайлом на этаже 1.
fn station(world: &mut World, initialized: bool) -> (Entity, Entity) {
    let (map, _) = world.create_map(initialized);
    let grid = world.create_grid(map);
    let floor = (0..4).flat_map(|x| (0..4).map(move |y| (TileCoord::new(x, y, 0), FLOOR)));
    world.set_tiles(grid, floor);
    world.set_tile(grid, TileCoord::new(0, 0, 1), FLOOR);
    (map, grid)
}

fn place(world: &mut World, parent: Entity, position: Vec2) -> Entity {
    let entity = world.spawn_entity();
    world
        .set_coordinates(entity, EntityCoordinates::new(parent, position), None)
        .unwrap();
    let map_init = world.is_on_initialized_map(entity);
    world.initialize_and_start(entity, map_init);
    entity
}

fn close(a: Vec2, b: Vec2) -> bool {
    (a - b).length() < 1e-4
}

#[test]
fn world_coordinates_follow_parent_chain_with_rotation_and_level() {
    let mut world = world();
    let (map, grid) = station(&mut world, true);
    world
        .set_coordinates(
            grid,
            EntityCoordinates::on_level(map, Vec2::new(10.0, 0.0), 2),
            Some(Angle::degrees(90.0)),
        )
        .unwrap();
    let crate_ = place(&mut world, grid, Vec2::new(1.0, 0.0));
    let item = place(&mut world, crate_, Vec2::new(0.0, 1.0));

    let transform = world.world_transform(item).unwrap();
    // Локальная точка (1, 1) грида, повёрнутого на 90°, — это (-1, 1) от его начала.
    assert!(
        close(transform.position, Vec2::new(9.0, 1.0)),
        "{transform:?}"
    );
    assert_eq!(transform.level, 2);
    let coordinates = world.map_coordinates(item).unwrap();
    assert_eq!((coordinates.map, coordinates.z), (MapId(1), 2));
    assert_eq!(world.get::<Transform>(item).unwrap().grid(), Some(grid));
    assert_eq!(world.get::<Transform>(item).unwrap().map(), Some(map));
    assert!(world.is_ancestor(grid, item) && !world.is_ancestor(item, grid));

    // Смена родителя сохраняет мировое положение.
    world.set_parent(item, map).unwrap();
    let after = world.world_transform(item).unwrap();
    assert!(close(after.position, Vec2::new(9.0, 1.0)));
    assert_eq!(after.level, 2);
    assert_eq!(world.get::<Transform>(item).unwrap().grid(), None);

    world
        .set_world_position(crate_, Vec2::new(10.0, 3.0))
        .unwrap();
    assert!(close(
        world.get::<Transform>(crate_).unwrap().local_position(),
        Vec2::new(3.0, 0.0)
    ));
}

#[test]
fn move_events_and_invalid_parents() {
    let mut world = world();
    world.subscribe::<Transform, MoveEvent>(|w, _, ev: &mut MoveEvent| {
        log(
            w,
            format!(
                "move {} parent_changed={}",
                ev.new_position,
                ev.parent_changed()
            ),
        );
    });
    world.subscribe::<Transform, EntParentChanged>(|w, _, _| log(w, "parent"));
    let (map, grid) = station(&mut world, true);
    take_log(&mut world);
    let a = place(&mut world, grid, Vec2::new(1.5, 1.5));
    let b = place(&mut world, a, Vec2::ZERO);
    assert_eq!(take_log(&mut world).len(), 4);

    world.set_local_position(a, Vec2::new(2.5, 1.5)).unwrap();
    world.set_local_position(a, Vec2::new(2.5, 1.5)).unwrap();
    assert_eq!(
        take_log(&mut world),
        ["move [2.5, 1.5] parent_changed=false"]
    );

    assert_eq!(
        world.set_coordinates(a, EntityCoordinates::new(b, Vec2::ZERO), None),
        Err(TransformError::Cycle)
    );
    assert_eq!(
        world.set_coordinates(a, EntityCoordinates::new(a, Vec2::ZERO), None),
        Err(TransformError::Cycle)
    );
    let bare = world.spawn_empty().id();
    assert_eq!(
        world.set_coordinates(a, EntityCoordinates::new(bare, Vec2::ZERO), None),
        Err(TransformError::BadParent)
    );
    assert!(take_log(&mut world).is_empty());

    world.detach_to_null(a);
    assert_eq!(world.parent(a), None);
    assert!(world.map_coordinates(b).is_none());
    assert_eq!(world.get::<Transform>(b).unwrap().map(), None);
    assert_eq!(world.parent(b), Some(a));
    let _ = map;
}

#[test]
fn mapping_map_is_paused_and_gets_map_init_on_initialization() {
    #[derive(Component)]
    struct Spawner;

    let mut world = world();
    world.subscribe::<Spawner, MapInit>(|w, _, _| log(w, "mapinit"));
    let (map, grid) = station(&mut world, false);
    assert!(world.get::<Map>(map).unwrap().paused);
    assert!(world.entity(grid).contains::<Paused>());

    let spawner = world.spawn_entity();
    world.entity_mut(spawner).insert(Spawner);
    world
        .set_coordinates(spawner, EntityCoordinates::new(grid, Vec2::ONE), None)
        .unwrap();
    let map_init = world.is_on_initialized_map(spawner);
    world.initialize_and_start(spawner, map_init);
    assert!(world.entity(spawner).contains::<Paused>());
    assert_eq!(
        world.get::<MetaData>(spawner).unwrap().stage,
        LifeStage::Initialized
    );
    assert!(take_log(&mut world).is_empty());

    world.initialize_map(map);
    assert_eq!(take_log(&mut world), ["mapinit"]);
    assert!(!world.entity(spawner).contains::<Paused>());
    assert!(!world.get::<Map>(map).unwrap().paused);
    world.initialize_map(map);
    assert!(take_log(&mut world).is_empty());

    // Переезд на карту на паузе замораживает сущность, обратно — размораживает.
    let (frozen, _) = world.create_map(false);
    world
        .set_coordinates(spawner, EntityCoordinates::new(frozen, Vec2::ZERO), None)
        .unwrap();
    assert!(world.entity(spawner).contains::<Paused>());
    world.set_map_paused(frozen, false);
    assert!(!world.entity(spawner).contains::<Paused>());
}

#[test]
fn maps_are_numbered_and_removed_with_contents() {
    let mut world = world();
    world.subscribe_broadcast::<MapRemoved>(|w, ev| log(w, format!("removed {}", ev.id.0)));
    let (first, id) = world.create_map(true);
    assert_eq!(id, MapId(1));
    assert_eq!(
        world.create_map_with_id(MapId(1), true),
        Err(MapError::Exists(MapId(1)))
    );
    assert_eq!(
        world.create_map_with_id(MapId(0), true),
        Err(MapError::Nullspace)
    );
    let fifth = world.create_map_with_id(MapId(5), true).unwrap();
    assert_eq!(world.create_map(true).1, MapId(6));
    assert_eq!(world.map_entity(MapId(5)), Some(fifth));

    let grid = world.create_grid(first);
    let item = place(&mut world, grid, Vec2::ZERO);
    assert_eq!(world.get::<Map>(first).unwrap().grids(), [grid]);
    world.delete_entity(grid);
    assert!(world.get::<Map>(first).unwrap().grids().is_empty());
    assert!(!world.is_alive(item));

    world.delete_entity(first);
    assert_eq!(take_log(&mut world), ["removed 1"]);
    assert!(world.map_entity(MapId(1)).is_none());
    assert_eq!(world.resource::<Maps>().len(), 2);
}

#[test]
fn tiles_raise_events_and_are_found_by_map_position() {
    let mut world = world();
    world.subscribe_broadcast::<TileChanged>(|w, ev| log(w, format!("tiles {}", ev.changes.len())));
    let (map, grid) = station(&mut world, true);
    assert_eq!(take_log(&mut world), ["tiles 16", "tiles 1"]);
    assert!(!world.set_tile(grid, TileCoord::new(0, 0, 0), FLOOR));
    assert!(!world.set_tile(map, TileCoord::new(0, 0, 0), FLOOR));

    world
        .set_coordinates(
            grid,
            EntityCoordinates::new(map, Vec2::new(100.0, 50.0)),
            None,
        )
        .unwrap();
    let at = |x: f32, y: f32, z: i16| MapCoordinates::on_level(MapId(1), Vec2::new(x, y), z);
    let tile = world.tile_at(at(102.5, 51.5, 0)).unwrap();
    assert_eq!((tile.grid, tile.coord), (grid, TileCoord::new(2, 1, 0)));
    assert!(world.tile_at(at(102.5, 51.5, 1)).is_none());
    assert_eq!(world.find_grid_at(at(100.5, 50.5, 1)), Some(grid));
    assert!(world.tile_at(at(0.0, 0.0, 0)).is_none());

    let on_grid = world.snap_to_grid_or_map(at(101.25, 50.75, 0)).unwrap();
    assert_eq!(on_grid.parent, grid);
    assert!(close(on_grid.position, Vec2::new(1.25, 0.75)));
    let in_space = world.snap_to_grid_or_map(at(5.0, 5.0, 3)).unwrap();
    assert_eq!((in_space.parent, in_space.z), (map, 3));

    let item = place(&mut world, grid, Vec2::new(3.2, 0.1));
    let nested = place(&mut world, item, Vec2::ZERO);
    assert_eq!(
        world.tile_of(nested).unwrap().coord,
        TileCoord::new(3, 0, 0)
    );
}

#[test]
fn anchoring_snaps_to_tile_and_registers_in_grid() {
    let mut world = world();
    world.subscribe::<Transform, AnchorStateChanged>(|w, _, ev: &mut AnchorStateChanged| {
        log(
            w,
            format!("anchored={} detaching={}", ev.anchored, ev.detaching),
        );
    });
    let (map, grid) = station(&mut world, true);
    let wall = place(&mut world, grid, Vec2::new(2.2, 3.9));
    assert!(world.anchor(wall));
    assert!(world.anchor(wall));
    let tile = TileCoord::new(2, 3, 0);
    assert_eq!(
        world.get::<MapGrid>(grid).unwrap().anchored_at(tile),
        [wall]
    );
    assert_eq!(
        world.get::<Transform>(wall).unwrap().local_position(),
        Vec2::new(2.5, 3.5)
    );
    assert_eq!(take_log(&mut world), ["anchored=true detaching=false"]);

    // Поворот привязку не снимает, перемещение — снимает.
    world
        .set_local_rotation(wall, Angle::degrees(90.0))
        .unwrap();
    assert!(world.get::<Transform>(wall).unwrap().anchored());
    world.set_local_position(wall, Vec2::new(0.5, 0.5)).unwrap();
    assert!(!world.get::<Transform>(wall).unwrap().anchored());
    assert!(
        world
            .get::<MapGrid>(grid)
            .unwrap()
            .anchored_at(tile)
            .is_empty()
    );
    assert_eq!(take_log(&mut world), ["anchored=false detaching=false"]);

    // В космосе привязаться не к чему.
    let debris = place(&mut world, map, Vec2::ZERO);
    assert!(!world.anchor(debris));

    world.anchor(wall);
    world.delete_entity(wall);
    assert!(
        world
            .get::<MapGrid>(grid)
            .unwrap()
            .anchored_at(TileCoord::new(0, 0, 0))
            .is_empty()
    );
    assert_eq!(
        take_log(&mut world),
        [
            "anchored=true detaching=false",
            "anchored=false detaching=true"
        ]
    );
}

const PROTOTYPES: &str = r#"
- type: entity
  id: Wall
  components:
  - type: Transform
    anchored: true
    noRot: true
- type: entity
  id: Locker
  components:
  - type: ContainerContainer
    containers:
      storage: !type:Container
        showEnts: true
      lock_slot: !type:ContainerSlot
- type: entity
  id: Crowbar
"#;

fn load_prototypes(world: &mut World) {
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str("test.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    assert_eq!(report.errors, []);
    assert!(report.unknown_components.is_empty(), "{report:?}");
    world.insert_resource(prototypes);
}

#[test]
fn prototypes_spawn_in_place_anchored_and_initialized() {
    let mut world = world();
    load_prototypes(&mut world);
    let (map, grid) = station(&mut world, true);

    let wall = world
        .spawn_on_map("Wall", MapCoordinates::new(MapId(1), Vec2::new(1.2, 2.7)))
        .unwrap();
    let transform = world.get::<Transform>(wall).unwrap();
    assert!(transform.anchored() && transform.no_rot());
    assert_eq!(transform.local_position(), Vec2::new(1.5, 2.5));
    assert_eq!(world.parent(wall), Some(grid));
    assert_eq!(
        world.get::<MetaData>(wall).unwrap().stage,
        LifeStage::MapInitialized
    );
    assert_eq!(
        world
            .get::<MapGrid>(grid)
            .unwrap()
            .anchored_at(TileCoord::new(1, 2, 0)),
        [wall]
    );
    world
        .set_local_rotation(wall, Angle::degrees(90.0))
        .unwrap();
    assert_eq!(
        world.get::<Transform>(wall).unwrap().local_rotation(),
        Angle::ZERO
    );

    // В космосе стена теряет привязку.
    let adrift = world
        .spawn_on_map("Wall", MapCoordinates::new(MapId(1), Vec2::new(50.0, 50.0)))
        .unwrap();
    assert_eq!(world.parent(adrift), Some(map));
    assert!(!world.get::<Transform>(adrift).unwrap().anchored());

    assert!(matches!(
        world.spawn_on_map("Missing", MapCoordinates::new(MapId(1), Vec2::ZERO)),
        Err(PlaceError::Spawn(_))
    ));
    assert_eq!(
        world.spawn_on_map("Wall", MapCoordinates::new(MapId(9), Vec2::ZERO)),
        Err(PlaceError::NoMap)
    );
    // Неудачное создание не оставляет сущность в мире.
    let count = |world: &mut World| world.query::<&MetaData>().iter(world).count();
    let before = count(&mut world);
    let bare = world.spawn_empty().id();
    assert!(matches!(
        world.spawn_attached("Wall", EntityCoordinates::new(bare, Vec2::ZERO)),
        Err(PlaceError::Location(TransformError::BadParent))
    ));
    assert_eq!(count(&mut world), before);
}

#[test]
fn containers_hold_entities_with_attempt_events() {
    let mut world = world();
    load_prototypes(&mut world);
    world.subscribe_broadcast::<EntInsertedIntoContainer>(|w, ev| {
        log(w, format!("inserted into {}", ev.container));
    });
    world.subscribe_broadcast::<EntRemovedFromContainer>(|w, ev| {
        log(w, format!("removed from {}", ev.container));
    });
    let (_, grid) = station(&mut world, true);
    let at = |x: f32| EntityCoordinates::new(grid, Vec2::new(x, 0.5));
    let locker = world.spawn_attached("Locker", at(1.5)).unwrap();
    let crowbar = world.spawn_attached("Crowbar", at(0.5)).unwrap();
    let second = world.spawn_attached("Crowbar", at(0.5)).unwrap();

    let storage = world.container(locker, "storage").unwrap();
    assert!(storage.show_ents && storage.kind == ContainerKind::List);
    assert_eq!(
        world.container(locker, "lock_slot").unwrap().kind,
        ContainerKind::Slot
    );

    world
        .insert_into_container(crowbar, locker, "lock_slot")
        .unwrap();
    assert_eq!(world.parent(crowbar), Some(locker));
    assert_eq!(world.containing_container(crowbar).unwrap().owner, locker);
    assert_eq!(
        world.insert_into_container(second, locker, "lock_slot"),
        Err(ContainerError::SlotFull)
    );
    assert_eq!(
        world.insert_into_container(second, locker, "missing"),
        Err(ContainerError::NoContainer)
    );
    assert_eq!(
        world.insert_into_container(locker, locker, "storage"),
        Err(ContainerError::Cycle)
    );
    assert_eq!(
        world.insert_into_container(grid, locker, "storage"),
        Err(ContainerError::NotInsertable)
    );

    // Перекладывание между контейнерами: вынута из одного, вставлена в другой.
    take_log(&mut world);
    world
        .insert_into_container(crowbar, locker, "storage")
        .unwrap();
    assert_eq!(
        take_log(&mut world),
        ["removed from lock_slot", "inserted into storage"]
    );
    assert!(world.container(locker, "lock_slot").unwrap().is_empty());

    // Обработчик может запретить вставку.
    world.subscribe::<ContainerManager, ContainerIsInsertingAttempt>(
        |_, _, ev: &mut ContainerIsInsertingAttempt| ev.cancelled = &*ev.container == "lock_slot",
    );
    assert_eq!(
        world.insert_into_container(second, locker, "lock_slot"),
        Err(ContainerError::Cancelled)
    );
    world
        .force_insert_into_container(second, locker, "lock_slot")
        .unwrap();

    // Вложенность: шкаф нельзя положить в его же содержимое.
    world.ensure_container(crowbar, "inside", ContainerKind::List);
    assert_eq!(
        world.insert_into_container(locker, crowbar, "inside"),
        Err(ContainerError::Cycle)
    );
    let nested = world.spawn_attached("Crowbar", at(0.5)).unwrap();
    world
        .insert_into_container(nested, crowbar, "inside")
        .unwrap();
    assert!(world.is_in_container_or_nested(nested));

    // Вынутая сущность оказывается рядом с владельцем на гриде.
    world.remove_from_container(crowbar, None).unwrap();
    assert_eq!(world.parent(crowbar), Some(grid));
    assert_eq!(
        world.get::<Transform>(crowbar).unwrap().local_position(),
        Vec2::new(1.5, 0.5)
    );
    assert_eq!(
        world.remove_from_container(crowbar, None),
        Err(ContainerError::NotContained)
    );

    // Удаление содержимого чистит список; удаление владельца удаляет содержимое.
    world.delete_entity(second);
    assert!(world.container(locker, "lock_slot").unwrap().is_empty());
    world
        .insert_into_container(crowbar, locker, "storage")
        .unwrap();
    world.delete_entity(locker);
    assert!(!world.is_alive(crowbar) && !world.is_alive(nested));
}

#[test]
fn moving_entity_out_of_owner_clears_container() {
    let mut world = world();
    load_prototypes(&mut world);
    let (_, grid) = station(&mut world, true);
    let locker = world
        .spawn_attached("Locker", EntityCoordinates::new(grid, Vec2::ONE))
        .unwrap();
    let crowbar = world
        .spawn_attached("Crowbar", EntityCoordinates::new(grid, Vec2::ONE))
        .unwrap();
    world
        .insert_into_container(crowbar, locker, "storage")
        .unwrap();
    world
        .set_coordinates(crowbar, EntityCoordinates::new(grid, Vec2::ZERO), None)
        .unwrap();
    assert!(world.containing_container(crowbar).is_none());
    assert!(world.container(locker, "storage").unwrap().is_empty());
    assert_eq!(world.empty_container(locker, "storage"), []);
}
