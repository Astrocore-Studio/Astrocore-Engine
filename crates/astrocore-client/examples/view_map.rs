//! Просмотр карты SS14 клиентом движка: сервер и клиент в одном процессе.
//!
//! Запуск:
//! `cargo run --release -p astrocore-client --example view_map -- <каталог Resources> [Maps/saltern.yml] [--capture кадр.png] [--walk]`
//!
//! Сервер загружает прототипы и карту, выдаёт игроку корги у первой точки появления;
//! клиент получает мир по сети (без UDP, пакеты передаются напрямую) и рисует его.
//! Управление: WASD или стрелки, Shift — шаг. `--capture` снимает кадр и завершает
//! программу; `--walk` ведёт персонажа вправо без клавиатуры.

use astrocore_client::{
    Capture, ClientPlugin, Game, GameResources, Link, register_client_components,
    register_draw_depth,
};
use astrocore_ecs::bevy_ecs::world::World;
use astrocore_ecs::{GameWorldExt, MetaData};
use astrocore_map::{
    EntityMigrations, MapCoordinates, MapLoadWorldExt, SpawnWorldExt, TileDefinitions,
    TransformWorldExt,
};
use astrocore_movement::{LocalInput, MoveButtons, MovementClientPlugin, MovementServerPlugin};
use astrocore_net::{
    ClientNet, ClientTransport, ClientWorldExt, ConnId, Incoming, NetClientPlugin, NetServerPlugin,
    ServerNet, SessionWorldExt,
};
use astrocore_physics::{PhysicsPlugin, register_collision_group};
use astrocore_prototypes::Prototypes;
use astrocore_resources::{Resources, ResourcesWorldExt};
use bevy::prelude::*;
use bevy::window::WindowResolution;

/// Группы столкновений SS14 (`Content.Shared/Physics/CollisionGroup.cs` @ 2f4a2aad).
fn register_ss14_collision_groups() {
    const OPAQUE: i32 = 1;
    const IMPASSABLE: i32 = 2;
    const MID: i32 = 4;
    const HIGH: i32 = 8;
    const LOW: i32 = 16;
    const BULLET: i32 = 64;
    const INTERACT: i32 = 128;
    const DOOR: i32 = 256;
    for (name, bits) in [
        ("None", 0),
        ("Opaque", OPAQUE),
        ("Impassable", IMPASSABLE),
        ("MidImpassable", MID),
        ("HighImpassable", HIGH),
        ("LowImpassable", LOW),
        ("GhostImpassable", 32),
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
    ] {
        register_collision_group(name, bits);
    }
}

/// Глубины отрисовки SS14 (`Content.Shared/DrawDepth/DrawDepth.cs` @ 2f4a2aad).
fn register_ss14_draw_depths() {
    for (name, depth) in [
        ("LowFloors", -22),
        ("ThickPipe", -21),
        ("ThickWire", -20),
        ("ThinPipe", -17),
        ("ThinWire", -16),
        ("BelowFloor", -15),
        ("FloorTiles", -14),
        ("FloorObjects", -13),
        ("Puddles", -12),
        ("HighFloorObjects", -11),
        ("DeadMobs", -10),
        ("SmallMobs", -9),
        ("Walls", -8),
        ("WallTops", -1),
        ("Objects", 0),
        ("SmallObjects", 1),
        ("WallMountedItems", 2),
        ("LargeObjects", 3),
        ("Items", 4),
        ("BelowMobs", 5),
        ("Mobs", 6),
        ("OverMobs", 7),
        ("Doors", 8),
        ("BlastDoors", 9),
        ("Overdoors", 10),
        ("Gasses", 17),
        ("Effects", 18),
        ("Ghosts", 19),
        ("Overlays", 20),
    ] {
        register_draw_depth(name, depth);
    }
}

/// Загружает прототипы из ресурсов и определения тайлов из прототипов вида `tile`.
fn load_content(world: &mut World, resources: &Resources, root: &str) {
    world.insert_resource(resources.clone());
    let mut report = world.load_prototypes_from_resources();
    let engine = std::path::Path::new(root).join("../RobustToolbox/Resources/EnginePrototypes");
    if engine.is_dir() {
        world.resource_scope(|world, mut prototypes: Mut<Prototypes>| {
            prototypes.load_dir(&engine, &mut report);
            prototypes.resolve(
                world.resource::<astrocore_ecs::ComponentRegistry>(),
                &mut report,
            );
        });
    }
    for error in report.errors.iter().take(5) {
        eprintln!("прототипы: {error}");
    }
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
    for (id, sprite, variants) in tiles {
        definitions.register(&id, sprite, variants);
    }
}

const CONN: ConnId = ConnId(1);

/// Сервер в том же процессе: пакеты передаются напрямую, сервер тикает вместе с клиентом.
struct LocalServer {
    world: World,
    attached: bool,
    player: bevy::prelude::Entity,
}

impl ClientTransport for LocalServer {
    fn receive(&mut self, client: &mut World) {
        let packets = std::mem::take(&mut self.world.resource_mut::<ServerNet>().outgoing);
        client
            .resource_mut::<ClientNet>()
            .incoming
            .extend(packets.into_iter().map(|packet| packet.bytes));
    }

    fn send(&mut self, client: &mut World) {
        let packets = std::mem::take(&mut client.resource_mut::<ClientNet>().outgoing);
        self.world.resource_mut::<ServerNet>().incoming.extend(
            packets
                .into_iter()
                .map(|(_, bytes)| Incoming::Packet(CONN, bytes)),
        );
        self.world.run_tick();
        let session = self.world.resource::<ServerNet>().session(CONN);
        if !self.attached
            && let Some(session) = session
        {
            self.world.attach_player(session, self.player);
            self.attached = true;
        }
    }
}

fn hold_right(mut game: ResMut<Game>) {
    if let Some(mut input) = game.0.get_resource_mut::<LocalInput>() {
        input.buttons = MoveButtons::RIGHT;
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str, takes_value: bool| -> Option<String> {
        let index = args.iter().position(|a| a == name)?;
        args.remove(index);
        Some(if takes_value {
            args.remove(index)
        } else {
            String::new()
        })
    };
    let capture = flag("--capture", true);
    let walk = flag("--walk", false).is_some();
    let root = args.first().cloned().expect("нужен каталог Resources");
    let map = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "Maps/saltern.yml".to_owned());

    register_ss14_collision_groups();
    register_ss14_draw_depths();
    let mut resources = Resources::default();
    resources.mount(&root);

    // Сервер.
    let mut server = World::new();
    server.add_plugin(NetServerPlugin);
    server.add_plugin(PhysicsPlugin::SERVER);
    server.add_plugin(MovementServerPlugin);
    // Наборы сетевых событий сервера и клиента совпадают: ввод и звук есть у обоих.
    server.add_plugin(astrocore_input::InputPlugin);
    server.add_plugin(astrocore_audio::AudioPlugin);
    server.add_plugin(astrocore_light::LightPlugin);
    load_content(&mut server, &resources, &root);
    if let Ok(text) = resources.read_to_string("migration.yml")
        && let Ok(node) = astrocore_data::parse_yaml(&text)
    {
        server.insert_resource(EntityMigrations::from_node(&node));
    }
    let text = resources.read_to_string(&map).expect("файл карты читается");
    let node = astrocore_data::parse_yaml(&text).expect("файл карты разбирается");
    let report = server.load_map(&node, true).expect("карта загружается");
    println!("Карта {map}: {} сущностей", report.entities);
    let (_, map_id) = report.maps[0];
    // Игрок появляется у первой точки появления карты.
    let spawn = server
        .query::<(bevy::prelude::Entity, &MetaData)>()
        .iter(&server)
        .filter(|(_, meta)| {
            meta.prototype
                .as_deref()
                .is_some_and(|p| p.starts_with("SpawnPoint"))
        })
        .map(|(entity, _)| entity)
        .min()
        .and_then(|entity| server.map_coordinates(entity))
        .unwrap_or(MapCoordinates::new(map_id, astrocore_math::Vec2::ZERO));
    let player = server
        .spawn_on_map("MobCorgi", spawn)
        .expect("игрок создаётся");

    // Клиент.
    let mut client = World::new();
    client.add_plugin(NetClientPlugin);
    client.add_plugin(PhysicsPlugin::CLIENT);
    client.add_plugin(MovementClientPlugin);
    register_client_components(&mut client);
    load_content(&mut client, &resources, &root);
    client.net_connect("Urist");

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Astrocore".into(),
                    resolution: WindowResolution::new(1280, 720),
                    ..default()
                }),
                ..default()
            })
            .set(ImagePlugin::default_nearest()),
    )
    .insert_resource(ClearColor(Color::BLACK))
    .insert_resource(Game(client))
    .insert_resource(GameResources(resources))
    .insert_non_send(Link(Box::new(LocalServer {
        world: server,
        attached: false,
        player,
    })))
    .add_plugins(ClientPlugin);
    if walk {
        app.add_systems(Update, hold_right);
    }
    if let Some(path) = capture {
        app.insert_resource(Capture {
            path: path.into(),
            after_frames: 90,
        });
    }
    app.run();
}
