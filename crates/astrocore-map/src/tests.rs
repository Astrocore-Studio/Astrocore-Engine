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

    // Смена родителя сохраняет мировое положение. Переход между гридами приостановлен:
    // иначе сущность над тайлом грида сразу вернулась бы к нему.
    world.resource_mut::<GridTraversal>().suspended += 1;
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

/// Тайлы чанка в записи формата 7: номер (4 байта), флаги, вариант, поворот.
fn chunk_base64(tile_at: impl Fn(i32, i32) -> (i32, u8)) -> String {
    let mut bytes = Vec::new();
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            let (id, variant) = tile_at(x, y);
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&[0, variant, 0]);
        }
    }
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for group in bytes.chunks(3) {
        let value =
            group.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - group.len()));
        for index in 0..=group.len() {
            text.push(ALPHABET[(value >> (18 - 6 * index)) as usize & 63] as char);
        }
    }
    text
}

fn map_file() -> String {
    let tiles = chunk_base64(|x, y| {
        if x < 4 && y < 2 {
            (7, (x % 3) as u8)
        } else {
            (0, 0)
        }
    });
    format!(
        r#"meta:
  format: 7
maps: [1]
grids: [2]
tilemap:
  0: Space
  7: FloorSteel
entities:
- proto: ""
  entities:
  - uid: 1
    components:
    - type: MetaData
      name: Test Station
    - type: Transform
    - type: Map
      mapPaused: True
  - uid: 2
    components:
    - type: MetaData
      name: grid
    - type: Transform
      pos: 10,20
      parent: 1
    - type: MapGrid
      chunks:
        -1,0:
          ind: -1,0
          tiles: {tiles}
          version: 7
- proto: Wall
  entities:
  - uid: 3
    components:
    - type: Transform
      rot: 1.5707963267948966 rad
      pos: -14.5,1.5
      parent: 2
- proto: Locker
  entities:
  - uid: 4
    components:
    - type: Transform
      pos: -15.5,0.5
      parent: 2
    - type: ContainerContainer
      containers:
        storage: !type:Container
          ents:
          - 5
          showEnts: False
          occludes: True
        lock_slot: !type:ContainerSlot
          ent: 6
- proto: Crowbar
  entities:
  - uid: 5
    components:
    - type: Transform
      parent: 4
  - uid: 6
    components:
    - type: Transform
      parent: 4
  - uid: 7
    components:
    - type: Transform
      parent: 99
- proto: NoSuchThing
  entities:
  - uid: 8
    components:
    - type: Transform
      pos: 0.5,0.5
      parent: 2
"#
    )
}

#[test]
fn loads_map_file_with_grid_entities_and_containers() {
    use astrocore_data::parse_yaml;

    let mut world = world();
    load_prototypes(&mut world);
    world
        .resource_mut::<TileDefinitions>()
        .register("FloorSteel", None, 3);
    let root = parse_yaml(&map_file()).unwrap();
    // Таблица переименований: старое имя стены ведёт к новому, удалённый прототип — никуда.
    let renamed = map_file().replace("proto: Wall", "proto: wallOld");
    world.insert_resource(EntityMigrations::from_node(
        &parse_yaml("wallOld: WallRenamed\nWallRenamed: Wall\nCrowbar: null\n").unwrap(),
    ));
    let migrated = world
        .load_map(&parse_yaml(&renamed).unwrap(), true)
        .unwrap();
    assert_eq!((migrated.entities, migrated.migrated_away), (4, 3));
    assert!(
        world
            .get::<Transform>(migrated.uids[&3])
            .unwrap()
            .anchored()
    );
    world.delete_entity(migrated.maps[0].0);
    world.remove_resource::<EntityMigrations>();

    let report = world.load_map(&root, true).unwrap();

    assert_eq!(report.entities, 7);
    assert_eq!(report.unknown_prototypes.get("NoSuchThing"), Some(&1));
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(report.errors[0].contains("родителя 99 нет"));
    assert!(report.unknown_tiles.is_empty());
    let (map, id) = report.maps[0];
    assert_eq!(
        world.get::<MetaData>(map).unwrap().name.as_deref(),
        Some("Test Station")
    );
    assert!(world.get::<Map>(map).unwrap().initialized);
    let grid = report.grids[0];
    assert_eq!(world.get::<Map>(map).unwrap().grids(), [grid]);

    // Чанк (-1, 0): тайлы с x от -16.
    let tiles = world.get::<MapGrid>(grid).unwrap();
    assert_eq!(tiles.tile_count(), 8);
    let steel = world
        .resource::<TileDefinitions>()
        .tile("FloorSteel")
        .unwrap()
        .type_id;
    let tile = tiles.tile(TileCoord::new(-15, 1, 0));
    assert_eq!((tile.type_id, tile.variant), (steel, 1));
    assert!(tiles.tile(TileCoord::new(-12, 0, 0)).is_empty());

    // Стена привязана к клетке, повёрнута и инициализирована на карте.
    let wall = report.uids[&3];
    let transform = world.get::<Transform>(wall).unwrap();
    assert!(transform.anchored());
    assert_eq!(transform.local_position(), Vec2::new(-14.5, 1.5));
    assert_eq!(
        transform.local_rotation(),
        Angle::ZERO,
        "noRot из прототипа сильнее поворота"
    );
    assert_eq!(tiles.anchored_at(TileCoord::new(-15, 1, 0)), [wall]);
    assert_eq!(
        world.get::<MetaData>(wall).unwrap().stage,
        LifeStage::MapInitialized
    );
    let coordinates = world.map_coordinates(wall).unwrap();
    assert_eq!(
        (coordinates.map, coordinates.position),
        (id, Vec2::new(-4.5, 21.5))
    );

    // Содержимое контейнеров.
    let locker = report.uids[&4];
    assert_eq!(
        world.container(locker, "storage").unwrap().entities(),
        [report.uids[&5]]
    );
    assert_eq!(
        world.container(locker, "lock_slot").unwrap().entities(),
        [report.uids[&6]]
    );
    assert_eq!(world.parent(report.uids[&5]), Some(locker));
    // Сущность без родителя в файле остаётся «нигде».
    assert_eq!(world.parent(report.uids[&7]), None);

    // Та же карта для маппинга: на паузе, без MapInit.
    let report = world.load_map(&root, false).unwrap();
    let (map, second_id) = report.maps[0];
    assert_ne!(second_id, id);
    let state = world.get::<Map>(map).unwrap();
    assert!(state.paused && !state.initialized);
    let wall = report.uids[&3];
    assert!(world.entity(wall).contains::<Paused>());
    assert_eq!(
        world.get::<MetaData>(wall).unwrap().stage,
        LifeStage::Initialized
    );

    assert_eq!(
        world
            .load_map(
                &parse_yaml("meta: {format: 3}\nentities: []").unwrap(),
                true
            )
            .err(),
        Some(MapLoadError::Version(3))
    );
    assert!(matches!(
        world.load_map(&parse_yaml("entities: []").unwrap(), true),
        Err(MapLoadError::Format(_))
    ));
}

#[test]
fn entities_change_grid_when_they_walk_off_and_on() {
    let mut world = world();
    let (map, station) = station(&mut world, true);
    // Шаттл: второй грид в десяти тайлах правее, повёрнутый на четверть оборота.
    let shuttle = world.create_grid(map);
    world
        .set_coordinates(
            shuttle,
            EntityCoordinates::new(map, Vec2::new(10.0, 0.0)),
            Some(Angle(std::f32::consts::FRAC_PI_2)),
        )
        .unwrap();
    world.set_tiles(
        shuttle,
        (0..2).flat_map(|x| (0..2).map(move |y| (TileCoord::new(x, y, 0), FLOOR))),
    );

    let walker = place(&mut world, station, Vec2::new(3.5, 1.5));
    let chair = place(&mut world, station, Vec2::new(2.5, 1.5));
    assert!(world.anchor(chair));
    assert_eq!(world.parent(walker), Some(station));

    // Шаг за край станции: родителем становится карта, место на карте то же.
    world
        .set_local_position(walker, Vec2::new(5.5, 1.5))
        .unwrap();
    assert_eq!(world.parent(walker), Some(map));
    assert_eq!(
        world.map_coordinates(walker).unwrap().position,
        Vec2::new(5.5, 1.5)
    );

    // Шаг на шаттл: родителем становится он, положение пересчитано в его оси.
    world
        .set_local_position(walker, Vec2::new(9.5, 0.5))
        .unwrap();
    assert_eq!(world.parent(walker), Some(shuttle));
    let on_map = world.map_coordinates(walker).unwrap().position;
    assert!((on_map - Vec2::new(9.5, 0.5)).length() < 1e-4, "{on_map:?}");
    let local = world.get::<Transform>(walker).unwrap().local_position();
    assert!((local - Vec2::new(0.5, 0.5)).length() < 1e-4, "{local:?}");

    // Обратно на станцию одним переносом.
    world
        .set_coordinates(
            walker,
            EntityCoordinates::new(map, Vec2::new(1.5, 1.5)),
            None,
        )
        .unwrap();
    assert_eq!(world.parent(walker), Some(station));

    // Приостановленный переход родителя не меняет; сущность в контейнере не переходит.
    world.resource_mut::<GridTraversal>().suspended += 1;
    world
        .set_local_position(walker, Vec2::new(7.5, 1.5))
        .unwrap();
    assert_eq!(world.parent(walker), Some(station));
    world.resource_mut::<GridTraversal>().suspended -= 1;
    world
        .set_local_position(walker, Vec2::new(7.6, 1.5))
        .unwrap();
    assert_eq!(world.parent(walker), Some(map));
    let item = place(&mut world, walker, Vec2::ZERO);
    world.set_local_position(item, Vec2::new(0.1, 0.0)).unwrap();
    assert_eq!(world.parent(item), Some(walker));
}

#[test]
fn saved_map_loads_back_into_the_same_map() {
    use astrocore_data::{parse_yaml, to_yaml_string};

    let mut world = world();
    load_prototypes(&mut world);
    world
        .resource_mut::<TileDefinitions>()
        .register("FloorSteel", None, 3);
    // Компонент, которого игра не знает, и неизвестный пропущенный компонент сохраняются как
    // были; второй ящик без поворота стены и не привязан, хотя прототип привязан.
    let file = map_file()
        .replace(
            "    - type: MapGrid\n",
            "    - type: DecalGrid\n      chunkCollection:\n        version: 2\n        nodes: []\n    - type: MapGrid\n",
        )
        .replace(
            "  - uid: 3\n    components:\n",
            "  - uid: 3\n    missingComponents:\n    - Airtight\n    components:\n",
        )
        .replace(
            "- proto: Locker\n",
            "  - uid: 9\n    components:\n    - type: Transform\n      pos: -13.5,0.5\n      parent: 2\n      anchored: False\n- proto: Locker\n",
        )
        // Лом вне карт (`nullspace`) сохраняется вместе с картой файла.
        .replace(
            "- proto: NoSuchThing\n",
            "  - uid: 12\n    components:\n    - type: Transform\n- proto: NoSuchThing\n",
        );
    let first = world.load_map(&parse_yaml(&file).unwrap(), false).unwrap();
    let saved = world
        .save_map(first.maps[0].0, MapSaveOptions::default())
        .unwrap();
    // Сущность с потерянным родителем и неизвестный прототип в карту не входят; сущности
    // сохраняют номера из файла.
    assert_eq!(saved.entities, 8);
    let text = to_yaml_string(&saved.document);
    assert!(text.contains("nullspace:\n- 12\n"), "{text}");
    assert!(text.contains("- uid: 9\n"), "{text}");
    let second = world.load_map(&parse_yaml(&text).unwrap(), false).unwrap();
    assert_eq!(second.entities, 8, "{text}");
    let outside = second.uids[&12];
    assert!(world.parent(outside).is_none());
    assert!(world.get::<Paused>(outside).is_some());
    assert!(second.errors.is_empty(), "{:?}", second.errors);

    let map = second.maps[0].0;
    assert_eq!(
        world.get::<MetaData>(map).unwrap().name.as_deref(),
        Some("Test Station")
    );
    assert!(world.get::<Map>(map).unwrap().paused);
    let grid = second.grids[0];
    let tiles = world.get::<MapGrid>(grid).unwrap();
    assert_eq!(tiles.tile_count(), 8);
    assert_eq!(tiles.tile(TileCoord::new(-15, 1, 0)).variant, 1);
    let preserved = world.get::<MapPreserved>(grid).unwrap();
    assert_eq!(preserved.components[0].type_name(), Some("DecalGrid"));

    let by_proto = |world: &mut World, id: &str| -> Vec<Entity> {
        let mut found: Vec<Entity> = world
            .query::<(Entity, &MetaData)>()
            .iter(world)
            .filter(|(e, meta)| {
                meta.prototype.as_deref() == Some(id)
                    && world
                        .parent(*e)
                        .is_some_and(|p| p == grid || world.parent(p) == Some(grid))
            })
            .map(|(e, _)| e)
            .collect();
        found.sort_by(|a, b| {
            let x = |e: &Entity| world.get::<Transform>(*e).unwrap().local_position().x;
            x(a).total_cmp(&x(b))
        });
        found
    };
    let walls = by_proto(&mut world, "Wall");
    assert_eq!(walls.len(), 2);
    assert!(world.get::<Transform>(walls[0]).unwrap().anchored());
    assert!(!world.get::<Transform>(walls[1]).unwrap().anchored());
    assert_eq!(
        world.get::<MapPreserved>(walls[0]).unwrap().missing,
        ["Airtight"]
    );
    let locker = by_proto(&mut world, "Locker")[0];
    assert_eq!(
        world.container(locker, "storage").unwrap().entities().len(),
        1
    );
    assert_eq!(
        world
            .container(locker, "lock_slot")
            .unwrap()
            .entities()
            .len(),
        1
    );

    // Сохранение устойчиво: загруженная заново карта сохраняется так же.
    let again = world.save_map(map, MapSaveOptions::default()).unwrap();
    let third = world.load_map(&again.document, false).unwrap();
    let last = world
        .save_map(third.maps[0].0, MapSaveOptions::default())
        .unwrap();
    assert_eq!(
        to_yaml_string(&again.document),
        to_yaml_string(&last.document)
    );

    // Инициализированную карту без разрешения не сохранить.
    let played = world.load_map(&parse_yaml(&file).unwrap(), true).unwrap();
    assert_eq!(
        world
            .save_map(played.maps[0].0, MapSaveOptions::default())
            .err(),
        Some(MapSaveError::Initialized)
    );
}

#[test]
fn maps_load_under_a_chosen_id_and_take_their_nullspace_along() {
    use astrocore_data::parse_yaml;
    use astrocore_ecs::LifecycleWorldExt;

    let mut world = world();
    load_prototypes(&mut world);
    let file = map_file().replace(
        "- proto: NoSuchThing\n",
        "  - uid: 12\n    components:\n    - type: Transform\n- proto: NoSuchThing\n",
    );
    let node = parse_yaml(&file).unwrap();
    let options = MapLoadOptions {
        initialize: false,
        map_id: Some(MapId(7)),
    };
    let report = world.load_map_with(&node, options).unwrap();
    assert_eq!(report.maps[0].1, MapId(7));
    assert_eq!(
        world.load_map_with(&node, options).err(),
        Some(MapLoadError::MapExists(MapId(7)))
    );
    let outside = report.uids[&12];
    world.delete_entity(report.maps[0].0);
    world.run_tick();
    assert!(world.map_entity(MapId(7)).is_none());
    assert!(world.get_entity(outside).is_err());
}

#[test]
fn map_made_in_game_saves_without_any_file_loaded_before() {
    let mut world = world();
    load_prototypes(&mut world);
    let (map, _) = world.create_map(false);
    let grid = world.create_grid(map);
    world.set_tiles(grid, [(TileCoord::new(0, 0, 0), Tile::new(1))]);
    let mut saved = world.save_map(map, MapSaveOptions::default()).unwrap();
    assert_eq!(saved.entities, 2);
    // Номера сущностей документа доступны игре (ADR-0019): карта и грид записаны под ними.
    assert_eq!(saved.uids.len(), 2);
    let uid = |entity| saved.uids[&entity].to_string();
    let map_uid = uid(map);
    let grid_uid = uid(grid);
    let root = saved.document.as_map().unwrap();
    let listed = |key: &str| -> Vec<String> {
        root.get(key)
            .and_then(|n| n.as_seq())
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(listed("maps"), [map_uid]);
    assert_eq!(listed("grids"), [grid_uid]);
    // Документ можно править на месте.
    let section = saved
        .document
        .as_map_mut()
        .unwrap()
        .get_mut("entities")
        .unwrap()
        .as_seq_mut()
        .unwrap();
    section.clear();
    assert!(
        saved
            .document
            .as_map()
            .unwrap()
            .get("entities")
            .unwrap()
            .as_seq()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn loading_raises_map_loaded_with_file_uids() {
    use astrocore_data::parse_yaml;
    use astrocore_ecs::EventWorldExt;

    #[derive(bevy_ecs::resource::Resource, Default)]
    struct Seen(Vec<(usize, usize, usize)>);

    let mut world = world();
    load_prototypes(&mut world);
    world.init_resource::<Seen>();
    world.subscribe_broadcast::<MapLoaded>(|world, event| {
        let seen = (event.maps.len(), event.grids.len(), event.uids.len());
        world.resource_mut::<Seen>().0.push(seen);
    });
    let report = world
        .load_map(&parse_yaml(&map_file()).unwrap(), false)
        .unwrap();
    assert_eq!(world.resource::<Seen>().0, [(1, 1, report.uids.len())]);
}
