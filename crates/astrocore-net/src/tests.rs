use astrocore_console::ConfigWorldExt;
use astrocore_data::DataFormat;
use astrocore_ecs::{
    ComponentRegistry, EventWorldExt, GameTick, GameWorldExt, LifecycleWorldExt, MetaData,
    RegistryWorldExt,
};
use astrocore_map::{
    ContainerWorldExt, EntityCoordinates, MapGrid, MapWorldExt, SpawnWorldExt, Tile, TileCoord,
    Transform, TransformWorldExt,
};
use astrocore_math::Vec2;
use astrocore_prototypes::{LoadReport, Prototypes};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Component, Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Door {
    open: bool,
}

#[derive(Component, Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Label {
    text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Chat {
    text: String,
}

const PROTOTYPES: &str = r#"
- type: entity
  id: Player
- type: entity
  id: Airlock
  components:
  - type: Transform
    anchored: true
  - type: Door
- type: entity
  id: Locker
  components:
  - type: ContainerContainer
    containers:
      storage: !type:Container
- type: entity
  id: Crowbar
"#;

fn setup(world: &mut World) {
    world.register_data_component::<Door>();
    world.register_data_component::<Label>();
    world.register_net_serde::<Door>();
    world.register_net_serde::<Label>();
    world.register_net_event::<Chat>();
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str("test.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    assert_eq!(report.errors, []);
    world.insert_resource(prototypes);
}

/// Сервер и клиент в одном процессе, соединённые напрямую.
struct Link {
    server: World,
    client: World,
    grid: Entity,
    player: Entity,
    /// Терять каждое N-е состояние, отправленное по ненадёжному каналу.
    lose_every: Option<u32>,
    unreliable_sent: u32,
}

const CONN: ConnId = ConnId(1);

impl Link {
    fn new() -> Self {
        let mut server = World::new();
        server.add_plugin(NetServerPlugin);
        setup(&mut server);
        let (map, _) = server.create_map(true);
        let grid = server.create_grid(map);
        let floor =
            (0..64).flat_map(|x| (0..8).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1))));
        server.set_tiles(grid, floor);
        let player = server
            .spawn_attached("Player", EntityCoordinates::new(grid, Vec2::new(2.5, 2.5)))
            .unwrap();

        let mut client = World::new();
        client.add_plugin(NetClientPlugin);
        setup(&mut client);
        Self {
            server,
            client,
            grid,
            player,
            lose_every: None,
            unreliable_sent: 0,
        }
    }

    /// Клиент входит, сервер отдаёт ему сущность игрока.
    fn join(&mut self) -> Entity {
        self.client.net_connect("Urist");
        self.pump(1);
        let session = self.server.resource::<ServerNet>().session(CONN).unwrap();
        self.server.attach_player(session, self.player);
        self.pump(4);
        session
    }

    /// Такты: пакеты клиента → сервер, такт сервера, пакеты сервера → клиент, такт клиента.
    fn pump(&mut self, ticks: u32) {
        for _ in 0..ticks {
            let outgoing = std::mem::take(&mut self.client.resource_mut::<ClientNet>().outgoing);
            let mut server_net = self.server.resource_mut::<ServerNet>();
            server_net.incoming.extend(
                outgoing
                    .into_iter()
                    .map(|(_, bytes)| Incoming::Packet(CONN, bytes)),
            );
            self.server.run_tick();
            let outgoing = std::mem::take(&mut self.server.resource_mut::<ServerNet>().outgoing);
            for packet in outgoing {
                if !packet.reliable {
                    self.unreliable_sent += 1;
                    if self
                        .lose_every
                        .is_some_and(|n| !self.unreliable_sent.is_multiple_of(n))
                    {
                        continue;
                    }
                }
                self.client
                    .resource_mut::<ClientNet>()
                    .incoming
                    .push(packet.bytes);
            }
            self.client.run_tick();
        }
    }

    /// Сущность клиента, соответствующая сущности сервера.
    fn on_client(&self, server_entity: Entity) -> Option<Entity> {
        let net = self.server.get::<NetEntity>(server_entity)?;
        self.client.resource::<NetEntities>().entity(*net)
    }

    fn spawn(&mut self, prototype: &str, x: f32, y: f32) -> Entity {
        self.server
            .spawn_attached(
                prototype,
                EntityCoordinates::new(self.grid, Vec2::new(x, y)),
            )
            .unwrap()
    }
}

#[test]
fn client_receives_world_around_player() {
    let mut link = Link::new();
    let airlock = link.spawn("Airlock", 4.2, 2.9);
    link.join();

    let net = link.client.resource::<ClientNet>();
    assert_eq!(net.status, ClientStatus::Connected);
    assert_eq!(net.tick_rate, 30);
    let player = link.on_client(link.player).unwrap();
    assert_eq!(net.attached, Some(player));

    // Карта, грид с тайлами и иерархия.
    let grid = link.on_client(link.grid).unwrap();
    let client_grid = link.client.get::<MapGrid>(grid).unwrap();
    assert_eq!(client_grid.tile_count(), 64 * 8);
    assert_eq!(link.client.parent(player), Some(grid));
    assert_eq!(
        link.client.map_id_of(player),
        link.server.map_id_of(link.player)
    );
    assert_eq!(
        link.client
            .get::<Transform>(player)
            .unwrap()
            .local_position(),
        Vec2::new(2.5, 2.5)
    );

    // Сущность из прототипа: привязана к клетке, инициализирована, с сетевым компонентом.
    let door = link.on_client(airlock).unwrap();
    let meta = link.client.get::<MetaData>(door).unwrap();
    assert_eq!(meta.prototype.as_deref(), Some("Airlock"));
    assert!(meta.initialized());
    assert!(link.client.get::<Transform>(door).unwrap().anchored());
    assert_eq!(client_grid.anchored_at(TileCoord::new(4, 2, 0)), [door]);
    assert_eq!(link.client.get::<Door>(door), Some(&Door { open: false }));
}

#[test]
fn changes_removals_and_deletions_reach_client() {
    let mut link = Link::new();
    let airlock = link.spawn("Airlock", 4.5, 2.5);
    let crowbar = link.spawn("Crowbar", 3.5, 3.5);
    link.join();
    let door = link.on_client(airlock).unwrap();
    let tool = link.on_client(crowbar).unwrap();

    link.server.get_mut::<Door>(airlock).unwrap().open = true;
    link.server.add_comp(
        crowbar,
        Label {
            text: "ломик".into(),
        },
    );
    link.server
        .set_local_position(crowbar, Vec2::new(5.5, 1.5))
        .unwrap();
    link.server
        .set_tile(link.grid, TileCoord::new(0, 0, 0), Tile::EMPTY);
    link.pump(2);
    assert!(link.client.get::<Door>(door).unwrap().open);
    assert_eq!(link.client.get::<Label>(tool).unwrap().text, "ломик");
    assert_eq!(
        link.client.get::<Transform>(tool).unwrap().local_position(),
        Vec2::new(5.5, 1.5)
    );
    let grid = link.on_client(link.grid).unwrap();
    assert!(
        link.client
            .get::<MapGrid>(grid)
            .unwrap()
            .tile(TileCoord::new(0, 0, 0))
            .is_empty()
    );

    link.server.remove_comp::<Label>(crowbar);
    link.server.unanchor(airlock);
    link.pump(2);
    assert!(link.client.get::<Label>(tool).is_none());
    assert!(!link.client.get::<Transform>(door).unwrap().anchored());

    link.server.delete_entity(crowbar);
    link.pump(2);
    assert!(link.client.get_entity(tool).is_err());
    assert_eq!(link.server.resource::<DeletionLog>().len(), 0);

    // Тихий такт: состояние без сущностей.
    link.pump(3);
    let session = link.server.resource::<ServerNet>().session(CONN).unwrap();
    let sync = &link.server.get::<Session>(session).unwrap().sync;
    assert!(sync.acked() > GameTick(5));
}

#[test]
fn far_entities_arrive_only_when_player_comes_near() {
    let mut link = Link::new();
    link.server.set_cvar(&PVS_RANGE, 10.0).unwrap();
    let far = link.spawn("Airlock", 60.5, 2.5);
    let near = link.spawn("Crowbar", 3.5, 2.5);
    link.join();
    assert!(link.on_client(near).is_some());
    assert!(link.on_client(far).is_none());

    link.server
        .set_local_position(link.player, Vec2::new(55.5, 2.5))
        .unwrap();
    link.pump(2);
    let far_client = link.on_client(far).unwrap();
    let near_client = link.on_client(near).unwrap();
    assert!(link.client.get::<Detached>(near_client).is_some());
    assert_eq!(link.client.parent(near_client), None);

    // Пока сущность не видна, она меняется на сервере; при возвращении клиент догоняет.
    link.server.add_comp(
        near,
        Label {
            text: "новый".into(),
        },
    );
    link.pump(2);
    assert!(link.client.get::<Label>(near_client).is_none());
    link.server
        .set_local_position(link.player, Vec2::new(2.5, 2.5))
        .unwrap();
    link.pump(2);
    assert!(link.client.get::<Detached>(near_client).is_none());
    assert_eq!(link.client.get::<Label>(near_client).unwrap().text, "новый");
    assert!(link.client.parent(near_client).is_some());
    assert!(link.client.get::<Detached>(far_client).is_some());

    // Принудительно видимая сущность приходит с любого расстояния.
    link.server.resource_mut::<PvsOverrides>().global.push(far);
    link.pump(2);
    assert!(link.client.get::<Detached>(far_client).is_none());
}

#[test]
fn container_contents_replicate_with_owner() {
    let mut link = Link::new();
    let locker = link.spawn("Locker", 4.5, 4.5);
    let crowbar = link.spawn("Crowbar", 4.5, 4.5);
    link.server
        .insert_into_container(crowbar, locker, "storage")
        .unwrap();
    link.join();
    let (owner, item) = (
        link.on_client(locker).unwrap(),
        link.on_client(crowbar).unwrap(),
    );
    assert_eq!(
        link.client.container(owner, "storage").unwrap().entities(),
        [item]
    );
    assert_eq!(link.client.containing_container(item).unwrap().owner, owner);
    assert_eq!(link.client.parent(item), Some(owner));

    link.server.remove_from_container(crowbar, None).unwrap();
    link.pump(2);
    assert!(link.client.container(owner, "storage").unwrap().is_empty());
    assert!(link.client.containing_container(item).is_none());
    assert_eq!(link.client.parent(item), link.on_client(link.grid));
}

#[test]
fn lost_packets_do_not_break_convergence() {
    let mut link = Link::new();
    let airlock = link.spawn("Airlock", 4.5, 2.5);
    link.join();
    let door = link.on_client(airlock).unwrap();
    // Доходит только каждое четвёртое состояние.
    link.lose_every = Some(4);
    for step in 0..40 {
        link.server.get_mut::<Door>(airlock).unwrap().open = step % 2 == 0;
        let x = 2.5 + step as f32 * 0.1;
        link.server
            .set_local_position(link.player, Vec2::new(x, 2.5))
            .unwrap();
        link.pump(1);
    }
    link.lose_every = None;
    link.pump(2);
    assert_eq!(
        link.client.get::<Door>(door),
        link.server.get::<Door>(airlock)
    );
    let player = link.on_client(link.player).unwrap();
    assert_eq!(
        link.client
            .get::<Transform>(player)
            .unwrap()
            .local_position(),
        link.server
            .get::<Transform>(link.player)
            .unwrap()
            .local_position()
    );
}

#[test]
fn entity_budget_spreads_join_over_several_ticks() {
    let mut link = Link::new();
    link.server.set_cvar(&PVS_ENTITY_BUDGET, 10).unwrap();
    let items: Vec<Entity> = (0..50)
        .map(|i| {
            link.spawn(
                "Crowbar",
                1.5 + (i % 10) as f32 * 0.5,
                1.5 + (i / 10) as f32,
            )
        })
        .collect();
    link.client.net_connect("Urist");
    link.pump(1);
    let session = link.server.resource::<ServerNet>().session(CONN).unwrap();
    link.server.attach_player(session, link.player);
    link.pump(2);
    let arrived = items
        .iter()
        .filter(|e| link.on_client(**e).is_some())
        .count();
    assert!(arrived > 0 && arrived < 50, "{arrived}");
    link.pump(10);
    assert!(items.iter().all(|e| link.on_client(*e).is_some()));
}

#[derive(Resource, Default)]
struct Heard(Vec<String>);

#[test]
fn net_events_travel_both_ways() {
    let mut link = Link::new();
    link.server.init_resource::<Heard>();
    link.client.init_resource::<Heard>();
    link.server.subscribe_net_event::<Chat>(|world, event| {
        assert!(event.session.is_some());
        world
            .resource_mut::<Heard>()
            .0
            .push(event.event.text.clone());
    });
    link.client.subscribe_net_event::<Chat>(|world, event| {
        assert!(event.session.is_none());
        world
            .resource_mut::<Heard>()
            .0
            .push(event.event.text.clone());
    });
    let session = link.join();
    link.client.send_net_event_to_server(&Chat {
        text: "привет".into(),
    });
    link.server.send_net_event(
        NetTarget::Session(session),
        &Chat {
            text: "добро пожаловать".into(),
        },
    );
    link.server.send_net_event(
        NetTarget::All,
        &Chat {
            text: "всем".into(),
        },
    );
    link.pump(2);
    assert_eq!(link.server.resource::<Heard>().0, ["привет"]);
    assert_eq!(
        link.client.resource::<Heard>().0,
        ["добро пожаловать", "всем"]
    );
}

#[test]
fn sessions_connect_attach_and_disconnect() {
    #[derive(Resource, Default)]
    struct Log(Vec<&'static str>);

    let mut link = Link::new();
    link.server.init_resource::<Log>();
    link.server
        .subscribe_broadcast::<PlayerConnected>(|w, _| w.resource_mut::<Log>().0.push("in"));
    link.server
        .subscribe_broadcast::<PlayerAttached>(|w, _| w.resource_mut::<Log>().0.push("attach"));
    link.server
        .subscribe_broadcast::<PlayerDetached>(|w, _| w.resource_mut::<Log>().0.push("detach"));
    link.server
        .subscribe_broadcast::<PlayerDisconnected>(|w, _| w.resource_mut::<Log>().0.push("out"));
    let session = link.join();
    assert_eq!(link.server.session_of(link.player), Some(session));
    assert_eq!(link.server.get::<Session>(session).unwrap().name, "Urist");

    // Удаление сущности игрока освобождает сессию; клиент узнаёт, что остался без тела.
    link.server.delete_entity(link.player);
    link.pump(2);
    assert_eq!(link.server.get::<Session>(session).unwrap().attached, None);
    assert_eq!(link.client.resource::<ClientNet>().attached, None);

    link.server
        .resource_mut::<ServerNet>()
        .incoming
        .push(Incoming::Disconnected(CONN));
    link.pump(1);
    assert!(link.server.get_entity(session).is_err());
    assert_eq!(
        link.server.resource::<Log>().0,
        ["in", "attach", "detach", "out"]
    );
}

#[test]
fn mismatched_component_sets_are_rejected() {
    #[derive(Component, Clone, Default, Serialize, Deserialize)]
    struct Extra;

    let mut link = Link::new();
    link.client.register_net_serde::<Extra>();
    link.client.net_connect("Urist");
    link.pump(1);
    assert!(matches!(
        link.client.resource::<ClientNet>().status,
        ClientStatus::Rejected(_)
    ));
    assert!(link.server.resource::<ServerNet>().session(CONN).is_none());
    // Мусор от клиента без сессии не роняет сервер.
    link.server
        .resource_mut::<ServerNet>()
        .incoming
        .push(Incoming::Packet(CONN, vec![0xff; 5]));
    link.pump(1);
}
