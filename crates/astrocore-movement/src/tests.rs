use std::collections::VecDeque;

use astrocore_data::DataFormat;
use astrocore_ecs::{ComponentRegistry, GameWorldExt};
use astrocore_map::{EntityCoordinates, MapWorldExt, SpawnWorldExt, Tile, TileCoord, Transform};
use astrocore_math::Vec2;
use astrocore_net::{
    ClientNet, ClientWorldExt, ConnId, Incoming, NetClientPlugin, NetEntities, NetEntity,
    NetServerPlugin, ServerNet, Session, SessionWorldExt,
};
use astrocore_physics::{PhysicsPlugin, register_collision_group};
use astrocore_prototypes::{LoadReport, Prototypes};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use super::*;

const PROTOTYPES: &str = r#"
- type: entity
  id: Wall
  components:
  - type: Transform
    anchored: true
  - type: Physics
    bodyType: Static
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeAabb {}
        layer: [ WallLayer ]
- type: entity
  id: Mob
  components:
  - type: InputMover
  - type: MovementSpeedModifier
  - type: Physics
    bodyType: KinematicController
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeCircle
          radius: 0.35
        density: 185
        mask: [ MobMask ]
        layer: [ MobLayer ]
"#;

fn setup(world: &mut World) {
    register_collision_group("WallLayer", 0b1111);
    register_collision_group("MobMask", 0b0110);
    register_collision_group("MobLayer", 0b10000);
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str("test.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    assert_eq!(report.errors, []);
    assert!(report.unknown_components.is_empty());
    world.insert_resource(prototypes);
}

const CONN: ConnId = ConnId(1);

/// Сервер и клиент в одном процессе с задержкой сети в тактах в каждую сторону.
struct Link {
    server: World,
    client: World,
    mob: Entity,
    delay: usize,
    to_server: VecDeque<Vec<Vec<u8>>>,
    to_client: VecDeque<Vec<Vec<u8>>>,
}

impl Link {
    fn new(delay: usize) -> Self {
        let mut server = World::new();
        server.add_plugin(NetServerPlugin);
        server.add_plugin(PhysicsPlugin::SERVER);
        server.add_plugin(MovementServerPlugin);
        setup(&mut server);
        let (map, _) = server.create_map(true);
        let grid = server.create_grid(map);
        let floor =
            (0..40).flat_map(|x| (0..10).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1))));
        server.set_tiles(grid, floor);
        let at = |x: f32, y: f32| EntityCoordinates::new(grid, Vec2::new(x, y));
        for y in 0..10 {
            server
                .spawn_attached("Wall", at(12.5, y as f32 + 0.5))
                .unwrap();
        }
        let mob = server.spawn_attached("Mob", at(3.5, 5.5)).unwrap();

        let mut client = World::new();
        client.add_plugin(NetClientPlugin);
        client.add_plugin(PhysicsPlugin::CLIENT);
        client.add_plugin(MovementClientPlugin);
        setup(&mut client);
        let mut link = Self {
            server,
            client,
            mob,
            delay,
            to_server: VecDeque::new(),
            to_client: VecDeque::new(),
        };
        link.client.net_connect("Urist");
        link.pump(delay as u32 + 2);
        let session = link.server.resource::<ServerNet>().session(CONN).unwrap();
        link.server.attach_player(session, mob);
        link.pump(2 * delay as u32 + 6);
        assert!(link.client.resource::<ClientNet>().attached.is_some());
        assert_eq!(link.client.resource::<ClientNet>().last_error, None);
        link
    }

    fn pump(&mut self, ticks: u32) {
        for _ in 0..ticks {
            // Клиент.
            if self.to_client.len() > self.delay
                && let Some(packets) = self.to_client.pop_front()
            {
                self.client
                    .resource_mut::<ClientNet>()
                    .incoming
                    .extend(packets);
            }
            self.client.run_tick();
            let sent = std::mem::take(&mut self.client.resource_mut::<ClientNet>().outgoing);
            self.to_server
                .push_back(sent.into_iter().map(|(_, bytes)| bytes).collect());
            // Сервер.
            if self.to_server.len() > self.delay
                && let Some(packets) = self.to_server.pop_front()
            {
                self.server.resource_mut::<ServerNet>().incoming.extend(
                    packets
                        .into_iter()
                        .map(|bytes| Incoming::Packet(CONN, bytes)),
                );
            }
            self.server.run_tick();
            let sent = std::mem::take(&mut self.server.resource_mut::<ServerNet>().outgoing);
            self.to_client
                .push_back(sent.into_iter().map(|packet| packet.bytes).collect());
        }
    }

    fn hold(&mut self, buttons: MoveButtons, ticks: u32) {
        self.client.resource_mut::<LocalInput>().buttons = buttons;
        self.pump(ticks);
    }

    fn server_position(&self) -> Vec2 {
        self.server
            .get::<Transform>(self.mob)
            .unwrap()
            .local_position()
    }

    fn client_position(&self) -> Vec2 {
        let net = *self.server.get::<NetEntity>(self.mob).unwrap();
        let local = self.client.resource::<NetEntities>().entity(net).unwrap();
        self.client
            .get::<Transform>(local)
            .unwrap()
            .local_position()
    }
}

#[test]
fn buttons_give_directions() {
    assert_eq!(MoveButtons::NONE.direction(), Vec2::ZERO);
    assert_eq!(MoveButtons::UP.direction(), Vec2::Y);
    assert_eq!(
        MoveButtons::LEFT.union(MoveButtons::RIGHT).direction(),
        Vec2::ZERO
    );
    let diagonal = MoveButtons::UP.union(MoveButtons::RIGHT).direction();
    assert!((diagonal.length() - 1.0).abs() < 1e-6 && diagonal.x > 0.0 && diagonal.y > 0.0);
}

#[test]
fn mover_reaches_sprint_speed_and_stops_quickly() {
    let mut link = Link::new(0);
    let (world, mob) = (&mut link.server, link.mob);
    world.get_mut::<InputMover>(mob).unwrap().held = MoveButtons::RIGHT;
    for _ in 0..10 {
        world.run_tick();
    }
    let velocity = world.get::<Physics>(mob).unwrap().linear_velocity;
    assert!(
        (velocity.x - 4.5).abs() < 1e-3 && velocity.y == 0.0,
        "{velocity}"
    );
    // Шаг вместо бега.
    world.get_mut::<InputMover>(mob).unwrap().held = MoveButtons::RIGHT.union(MoveButtons::WALK);
    for _ in 0..10 {
        world.run_tick();
    }
    assert!((world.get::<Physics>(mob).unwrap().linear_velocity.x - 2.5).abs() < 1e-3);
    // Клавиша отпущена: трение останавливает за несколько тактов, тело засыпает.
    world.get_mut::<InputMover>(mob).unwrap().held = MoveButtons::NONE;
    for _ in 0..20 {
        world.run_tick();
    }
    let body = world.get::<Physics>(mob).unwrap();
    assert!(
        body.linear_velocity == Vec2::ZERO && !body.awake,
        "{body:?}"
    );
    // Запрет движения.
    let mut mover = world.get_mut::<InputMover>(mob).unwrap();
    mover.can_move = false;
    mover.held = MoveButtons::UP;
    world.run_tick();
    assert_eq!(
        world.get::<Physics>(mob).unwrap().linear_velocity,
        Vec2::ZERO
    );
}

#[test]
fn prediction_leads_server_and_converges_under_latency() {
    for delay in [0usize, 3, 8] {
        let mut link = Link::new(delay);
        let start = link.server_position();
        assert_eq!(link.client_position(), start);

        // Первый же такт двигает персонажа на клиенте, сервер узнаёт позже.
        link.hold(MoveButtons::RIGHT, 1);
        assert!(link.client_position().x > start.x, "задержка {delay}");
        if delay > 0 {
            assert_eq!(link.server_position(), start);
        }

        link.hold(MoveButtons::RIGHT, 30);
        let (client, server) = (link.client_position(), link.server_position());
        // Клиент впереди сервера на путь за время, которое ввод идёт до сервера.
        let lead = client.x - server.x;
        let expected = 4.5 * delay as f32 / 30.0;
        assert!(
            (lead - expected).abs() < 0.35,
            "задержка {delay}: опережение {lead}, ждали {expected}"
        );
        assert!(link.client.resource::<Prediction>().pending() <= 2 * delay + 3);

        // Игрок остановился: после задержки клиент и сервер совпадают.
        link.hold(MoveButtons::NONE, 2 * delay as u32 + 30);
        let (client, server) = (link.client_position(), link.server_position());
        assert!(
            (client - server).length() < 1e-3,
            "задержка {delay}: {client} и {server}"
        );
        assert!(server.x > start.x + 4.0);
    }
}

#[test]
fn predicted_mob_is_stopped_by_wall_on_both_sides() {
    let mut link = Link::new(4);
    link.hold(MoveButtons::RIGHT, 120);
    link.hold(MoveButtons::NONE, 40);
    let (client, server) = (link.client_position(), link.server_position());
    // Стена на x = 12: моб радиусом 0,35 упирается в неё.
    assert!((server.x - 11.65).abs() < 0.03, "{server}");
    assert!((client - server).length() < 1e-3, "{client} и {server}");
    let session = link.server.resource::<ServerNet>().session(CONN).unwrap();
    assert!(link.server.get::<Session>(session).unwrap().last_input > 100);
}
