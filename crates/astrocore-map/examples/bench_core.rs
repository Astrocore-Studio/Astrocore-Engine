//! Замер ядра: загрузка прототипов, создание сущностей на гриде и такт с движением игроков.
//!
//! Запуск: `cargo run --release -p astrocore-map --example bench_core -- <каталог прототипов> [сущностей] [игроков]`
//! Каталог — `Resources/Prototypes` SS14 или свой. Числа по умолчанию: 20 000 сущностей
//! и 200 движущихся игроков.

use std::path::Path;
use std::time::Instant;

use astrocore_ecs::bevy_ecs::prelude::*;
use astrocore_ecs::{EventWorldExt, GameRng, GameTime, GameWorldExt, Update};
use astrocore_map::{
    EntityCoordinates, MapPlugin, MapWorldExt, MoveEvent, SpawnWorldExt, Tile, TileCoord,
    Transform, TransformWorldExt,
};
use astrocore_math::Vec2;
use astrocore_prototypes::{PrototypeWorldExt, Prototypes};

#[derive(Component)]
struct Player {
    velocity: Vec2,
}

#[derive(Resource, Default)]
struct Moves(u64);

const SIDE: i32 = 256;

fn move_players(world: &mut World) {
    let dt = world.resource::<GameTime>().frame_time();
    let moves: Vec<(Entity, Vec2)> = world
        .query::<(Entity, &Player, &Transform)>()
        .iter(world)
        .map(|(entity, player, transform)| {
            let mut next = transform.local_position() + player.velocity * dt;
            next.x = next.x.rem_euclid(SIDE as f32);
            next.y = next.y.rem_euclid(SIDE as f32);
            (entity, next)
        })
        .collect();
    for (entity, position) in moves {
        let _ = world.set_local_position(entity, position);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("нужен каталог прототипов");
    let entities: usize = args
        .next()
        .map_or(20_000, |a| a.parse().expect("число сущностей"));
    let players: usize = args
        .next()
        .map_or(200, |a| a.parse().expect("число игроков"));

    let mut world = World::new();
    world.add_plugin(MapPlugin);
    world.insert_resource(GameRng::seeded(1));
    world.init_resource::<Moves>();
    // Десять подписчиков на перемещение — как у систем, следящих за положением.
    for _ in 0..10 {
        world.subscribe::<Transform, MoveEvent>(|world, _, _| world.resource_mut::<Moves>().0 += 1);
    }
    world.add_game_systems(Update, move_players);

    let started = Instant::now();
    let report = world.load_prototypes(Path::new(&dir));
    println!(
        "Прототипы: {} файлов, {} сущностей, ошибок {} — {:.0?}",
        report.files,
        report.loaded.get("entity").copied().unwrap_or(0),
        report.errors.len(),
        started.elapsed()
    );

    let (map, _) = world.create_map(true);
    let grid = world.create_grid(map);
    let started = Instant::now();
    let floor =
        (0..SIDE).flat_map(|x| (0..SIDE).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1))));
    world.set_tiles(grid, floor);
    println!("Грид {SIDE}×{SIDE}: {:.1?}", started.elapsed());

    let ids: Vec<String> = world
        .resource::<Prototypes>()
        .entities()
        .map(|p| p.id.to_string())
        .collect();
    assert!(!ids.is_empty(), "в каталоге нет прототипов сущностей");
    let started = Instant::now();
    for index in 0..entities {
        let (x, y) = {
            let mut rng = world.resource_mut::<GameRng>();
            (
                rng.range_f32(0.0, SIDE as f32),
                rng.range_f32(0.0, SIDE as f32),
            )
        };
        let id = &ids[index % ids.len()];
        world
            .spawn_attached(id, EntityCoordinates::new(grid, Vec2::new(x, y)))
            .expect("прототип создаётся");
    }
    let spawn = started.elapsed();
    println!(
        "Создание {entities} сущностей: {spawn:.0?} ({:.1} мкс на сущность)",
        spawn.as_secs_f64() * 1e6 / entities as f64
    );

    for index in 0..players {
        let angle = index as f32;
        let player = world
            .spawn_attached(
                &ids[0],
                EntityCoordinates::new(grid, Vec2::splat(SIDE as f32 / 2.0)),
            )
            .expect("игрок создаётся");
        world.entity_mut(player).insert(Player {
            velocity: Vec2::new(angle.cos(), angle.sin()) * 4.0,
        });
    }
    let ticks = 300;
    let mut worst = std::time::Duration::ZERO;
    let started = Instant::now();
    for _ in 0..ticks {
        let tick = Instant::now();
        world.run_tick();
        worst = worst.max(tick.elapsed());
    }
    let total = started.elapsed();
    println!(
        "Такт с {players} игроками: в среднем {:.1} мкс, худший {:.1} мкс, событий перемещения {}",
        total.as_secs_f64() * 1e6 / f64::from(ticks),
        worst.as_secs_f64() * 1e6,
        world.resource::<Moves>().0
    );
}
