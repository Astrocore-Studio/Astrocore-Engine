//! Загрузка карты SS14 в мир сервера: проверка загрузчика на настоящих данных и замер такта.
//!
//! Запуск:
//! `cargo run --release -p astrocore-movement --example load_ss14_map -- <каталог Resources> [Maps/saltern.yml]`

use std::time::Instant;

use astrocore_ecs::bevy_ecs::prelude::*;
use astrocore_ecs::{GameWorldExt, MetaData};
use astrocore_map::{EntityMigrations, MapGrid, MapLoadWorldExt, TileDefinitions, Transform};
use astrocore_movement::MovementServerPlugin;
use astrocore_net::NetServerPlugin;
use astrocore_physics::{Fixtures, Physics, PhysicsPlugin, register_collision_group};
use astrocore_prototypes::Prototypes;
use astrocore_resources::{Resources, ResourcesWorldExt};

/// Группы столкновений SS14 (`Content.Shared/Physics/CollisionGroup.cs` @ 2f4a2aad).
fn register_ss14_collision_groups() {
    const OPAQUE: i32 = 1 << 0;
    const IMPASSABLE: i32 = 1 << 1;
    const MID: i32 = 1 << 2;
    const HIGH: i32 = 1 << 3;
    const LOW: i32 = 1 << 4;
    const GHOST: i32 = 1 << 5;
    const BULLET: i32 = 1 << 6;
    const INTERACT: i32 = 1 << 7;
    const DOOR: i32 = 1 << 8;
    let groups = [
        ("None", 0),
        ("Opaque", OPAQUE),
        ("Impassable", IMPASSABLE),
        ("MidImpassable", MID),
        ("HighImpassable", HIGH),
        ("LowImpassable", LOW),
        ("GhostImpassable", GHOST),
        ("BulletImpassable", BULLET),
        ("InteractImpassable", INTERACT),
        ("DoorPassable", DOOR),
        ("MapGrid", 1 << 30),
        ("AllMask", -1),
        (
            "SingularityLayer",
            OPAQUE | IMPASSABLE | MID | HIGH | LOW | BULLET | INTERACT | DOOR,
        ),
        ("MobMask", IMPASSABLE | HIGH | MID | LOW),
        ("MobLayer", OPAQUE | BULLET),
        ("SmallMobMask", IMPASSABLE | LOW),
        ("SmallMobLayer", OPAQUE | BULLET),
        ("FlyingMobMask", IMPASSABLE | HIGH),
        ("FlyingMobLayer", OPAQUE | BULLET),
        ("LargeMobMask", IMPASSABLE | HIGH | MID | LOW),
        ("LargeMobLayer", OPAQUE | HIGH | MID | LOW | BULLET),
        ("MachineMask", IMPASSABLE | MID | LOW),
        ("MachineLayer", OPAQUE | MID | LOW | BULLET),
        ("ConveyorMask", IMPASSABLE | MID | LOW | DOOR),
        ("CrateMask", IMPASSABLE | HIGH | LOW),
        ("TableMask", IMPASSABLE | MID),
        ("TableLayer", MID),
        ("TabletopMachineMask", IMPASSABLE | HIGH),
        ("TabletopMachineLayer", OPAQUE | BULLET),
        ("GlassAirlockLayer", HIGH | MID | BULLET | INTERACT),
        ("AirlockLayer", OPAQUE | HIGH | MID | BULLET | INTERACT),
        ("HumanoidBlockLayer", HIGH | MID),
        ("SlipLayer", MID | LOW),
        ("ItemMask", IMPASSABLE | HIGH),
        ("ThrownItem", IMPASSABLE | HIGH | BULLET),
        (
            "WallLayer",
            OPAQUE | IMPASSABLE | HIGH | MID | LOW | BULLET | INTERACT,
        ),
        (
            "GlassLayer",
            IMPASSABLE | HIGH | MID | LOW | BULLET | INTERACT,
        ),
        ("HalfWallLayer", MID | LOW),
        ("FlimsyLayer", OPAQUE | HIGH | MID | LOW | INTERACT),
        ("SpecialWallLayer", OPAQUE | HIGH | MID | LOW | BULLET),
        ("FullTileMask", IMPASSABLE | HIGH | MID | LOW | INTERACT),
        (
            "FullTileLayer",
            OPAQUE | HIGH | MID | LOW | BULLET | INTERACT,
        ),
        ("SubfloorMask", IMPASSABLE | LOW),
    ];
    for (name, bits) in groups {
        register_collision_group(name, bits);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("нужен каталог Resources");
    let map = args.next().unwrap_or_else(|| "Maps/saltern.yml".to_owned());

    register_ss14_collision_groups();
    let mut world = World::new();
    world.add_plugin(NetServerPlugin);
    world.add_plugin(PhysicsPlugin::SERVER);
    world.add_plugin(MovementServerPlugin);
    let mut resources = Resources::default();
    resources.mount(&root);
    // Прототипы движка Robust лежат рядом с контентом: `RobustToolbox/Resources`.
    let engine = std::path::Path::new(&root).join("../RobustToolbox/Resources/EnginePrototypes");
    world.insert_resource(resources.clone());
    if let Ok(text) = resources.read_to_string("migration.yml")
        && let Ok(node) = astrocore_data::parse_yaml(&text)
    {
        world.insert_resource(EntityMigrations::from_node(&node));
    }

    let started = Instant::now();
    let mut report = world.load_prototypes_from_resources();
    if engine.is_dir() {
        // Прототипы движка догружаются поверх и разрешаются вместе с остальными.
        world.resource_scope(|world, mut prototypes: Mut<Prototypes>| {
            prototypes.load_dir(&engine, &mut report);
            prototypes.resolve(
                world.resource::<astrocore_ecs::ComponentRegistry>(),
                &mut report,
            );
        });
    }
    println!(
        "Прототипы: {} сущностей, ошибок {}, незнакомых компонентов {} — {:.0?}",
        report.loaded.get("entity").copied().unwrap_or(0),
        report.errors.len(),
        report.unknown_components.len(),
        started.elapsed()
    );
    for error in report.errors.iter().take(10) {
        println!("  {error}");
    }

    // Определения тайлов — прототипы вида `tile`.
    let tiles: Vec<(String, Option<String>, u8)> = {
        let prototypes = world.resource::<Prototypes>();
        prototypes
            .ids("tile")
            .filter_map(|id| {
                let node = prototypes.resolved_node("tile", id)?;
                let sprite = node
                    .get("sprite")
                    .and_then(|n| n.as_str())
                    .map(str::to_owned);
                let variants = node
                    .get("variants")
                    .and_then(|n| n.as_str())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1);
                Some((id.to_owned(), sprite, variants))
            })
            .collect()
    };
    let mut definitions = world.resource_mut::<TileDefinitions>();
    for (id, sprite, variants) in &tiles {
        definitions.register(id, sprite.clone(), *variants);
    }
    println!("Определений тайлов: {}", definitions.len());

    let started = Instant::now();
    let text = resources.read_to_string(&map).expect("файл карты читается");
    let node = astrocore_data::parse_yaml(&text).expect("файл карты разбирается");
    let parsed = started.elapsed();
    let report = world.load_map(&node, true).expect("карта загружается");
    println!(
        "Карта {map}: разбор {parsed:.0?}, загрузка {:.0?}; сущностей {}, карт {}, гридов {}",
        started.elapsed() - parsed,
        report.entities,
        report.maps.len(),
        report.grids.len()
    );
    println!(
        "  пропущено по неизвестным прототипам: {} ({} видов), удалено переименованиями: {}, неизвестных тайлов: {}, ошибок: {}",
        report.unknown_prototypes.values().sum::<usize>(),
        report.unknown_prototypes.len(),
        report.migrated_away,
        report.unknown_tiles.len(),
        report.errors.len()
    );
    for error in report.errors.iter().take(10) {
        println!("  {error}");
    }
    for (prototype, count) in &report.unknown_prototypes {
        println!("  нет прототипа {prototype}: {count}");
    }
    for &grid in &report.grids {
        let tiles = world.get::<MapGrid>(grid).map_or(0, MapGrid::tile_count);
        println!("  грид: {tiles} тайлов");
    }
    let bodies = world.query::<&Physics>().iter(&world).count();
    let fixtures = world.query::<&Fixtures>().iter(&world).count();
    let anchored = world
        .query::<&Transform>()
        .iter(&world)
        .filter(|transform| transform.anchored())
        .count();
    let total = world.query::<&MetaData>().iter(&world).count();
    println!(
        "  в мире: {total} сущностей, тел {bodies}, с фикстурами {fixtures}, привязано к клеткам {anchored}"
    );

    let started = Instant::now();
    for _ in 0..100 {
        world.run_tick();
    }
    println!(
        "Такт сервера без игроков: {:.0} мкс",
        started.elapsed().as_secs_f64() * 1e6 / 100.0
    );
}
