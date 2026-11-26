//! Замер сети сервера: сборка состояний для многих игроков.
//!
//! Запуск: `cargo run --release -p astrocore-net --example bench_net -- [игроков] [сущностей] [тактов]`
//! По умолчанию 200 игроков, 20 000 сущностей на гриде 256×256, 300 тактов. Клиенты
//! условные: они подтверждают каждое состояние, но мир у себя не строят.

use std::time::{Duration, Instant};

use astrocore_ecs::bevy_ecs::prelude::*;
use astrocore_ecs::{GameRng, GameWorldExt, LifecycleWorldExt};
use astrocore_map::{
    EntityCoordinates, MapWorldExt, Tile, TileCoord, Transform, TransformWorldExt,
};
use astrocore_math::Vec2;
use astrocore_net::{
    ClientMessage, ConnId, Incoming, NetRegistry, NetRegistryWorldExt, NetServerPlugin,
    ServerMessage, ServerNet, SessionWorldExt,
};
use serde::{Deserialize, Serialize};

const SIDE: i32 = 256;

#[derive(Component, Clone, Default, Serialize, Deserialize)]
struct Item {
    charge: f32,
}

#[derive(Component)]
struct Walker(Vec2);

fn arg(index: usize, default: usize) -> usize {
    std::env::args()
        .nth(index)
        .map_or(default, |a| a.parse().expect("число"))
}

fn place(world: &mut World, grid: Entity, position: Vec2) -> Entity {
    let entity = world.spawn_entity();
    world.entity_mut(entity).insert(Item::default());
    let _ = world.set_coordinates(entity, EntityCoordinates::new(grid, position), None);
    world.initialize_and_start(entity, true);
    entity
}

fn main() {
    let (players, entities, ticks) = (arg(1, 200), arg(2, 20_000), arg(3, 300));
    let mut world = World::new();
    world.add_plugin(NetServerPlugin);
    world.register_net_serde::<Item>();
    world.insert_resource(GameRng::seeded(7));

    let (map, _) = world.create_map(true);
    let grid = world.create_grid(map);
    let floor =
        (0..SIDE).flat_map(|x| (0..SIDE).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1))));
    world.set_tiles(grid, floor);
    let random_point = |world: &mut World| {
        let mut rng = world.resource_mut::<GameRng>();
        Vec2::new(
            rng.range_f32(0.0, SIDE as f32),
            rng.range_f32(0.0, SIDE as f32),
        )
    };
    let mut items = Vec::with_capacity(entities);
    for _ in 0..entities {
        let position = random_point(&mut world);
        items.push(place(&mut world, grid, position));
    }

    let fingerprint = world.resource::<NetRegistry>().fingerprint();
    for index in 0..players {
        let hello = postcard::to_allocvec(&ClientMessage::Hello {
            name: format!("bot{index}"),
            fingerprint,
        })
        .expect("сообщение кодируется");
        world
            .resource_mut::<ServerNet>()
            .incoming
            .push(Incoming::Packet(ConnId(index as u64), hello));
    }
    world.run_tick();
    for index in 0..players {
        let position = random_point(&mut world);
        let body = place(&mut world, grid, position);
        let angle = index as f32;
        world
            .entity_mut(body)
            .insert(Walker(Vec2::new(angle.cos(), angle.sin()) * 4.0));
        let session = world
            .resource::<ServerNet>()
            .session(ConnId(index as u64))
            .expect("сессия создана");
        world.attach_player(session, body);
    }

    let (mut total, mut worst) = (Duration::ZERO, Duration::ZERO);
    let (mut bytes, mut join_bytes) = (0usize, 0usize);
    let warmup = 60;
    for tick in 0..ticks + warmup {
        // Игроки идут, каждый тик меняется 1 % предметов.
        let walkers: Vec<(Entity, Vec2)> = world
            .query::<(Entity, &Walker, &Transform)>()
            .iter(&world)
            .map(|(entity, walker, transform)| {
                let next = transform.local_position() + walker.0 / 30.0;
                (
                    entity,
                    Vec2::new(
                        next.x.rem_euclid(SIDE as f32),
                        next.y.rem_euclid(SIDE as f32),
                    ),
                )
            })
            .collect();
        for (entity, position) in walkers {
            let _ = world.set_local_position(entity, position);
        }
        for _ in 0..entities / 100 {
            let index = world.resource_mut::<GameRng>().below(items.len() as u32) as usize;
            if let Some(mut item) = world.get_mut::<Item>(items[index]) {
                item.charge += 1.0;
            }
        }

        let started = Instant::now();
        world.run_tick();
        let elapsed = started.elapsed();

        let outgoing = std::mem::take(&mut world.resource_mut::<ServerNet>().outgoing);
        let mut acks = Vec::new();
        for packet in outgoing {
            if tick >= warmup {
                bytes += packet.bytes.len();
            } else {
                join_bytes += packet.bytes.len();
            }
            // Условный клиент подтверждает состояния, пришедшие одним пакетом.
            if let Ok(ServerMessage::State(state)) = postcard::from_bytes(&packet.bytes) {
                acks.push((packet.conn, state.to));
            }
        }
        // Составные состояния (вход в игру) подтверждаются по текущему тику сервера.
        let current =
            astrocore_ecs::GameTick(world.resource::<astrocore_ecs::GameTime>().tick.0 - 1);
        let mut net = world.resource_mut::<ServerNet>();
        for index in 0..players {
            let conn = ConnId(index as u64);
            let tick_to_ack = acks
                .iter()
                .rev()
                .find(|(c, _)| *c == conn)
                .map_or(current, |(_, tick)| *tick);
            let ack = postcard::to_allocvec(&ClientMessage::Ack(tick_to_ack)).expect("кодируется");
            net.incoming.push(Incoming::Packet(conn, ack));
        }
        if tick >= warmup {
            total += elapsed;
            worst = worst.max(elapsed);
        }
    }
    let per_tick = total.as_secs_f64() * 1e3 / ticks as f64;
    println!("Игроков: {players}, сущностей: {entities}, тактов: {ticks}");
    println!(
        "Такт сервера с сетью: в среднем {per_tick:.2} мс, худший {:.2} мс (бюджет 33 мс)",
        worst.as_secs_f64() * 1e3
    );
    println!(
        "Трафик в игре: {:.1} КБ/с на игрока; вход в игру: {:.0} КБ на игрока",
        bytes as f64 / 1024.0 / players as f64 / (ticks as f64 / 30.0),
        join_bytes as f64 / 1024.0 / players as f64
    );
}
