use astrocore_data::DataFormat;
use astrocore_ecs::ComponentRegistry;
use astrocore_map::{EntityCoordinates, MapWorldExt, SpawnWorldExt, Tile, TileCoord};
use astrocore_math::Vec2;
use astrocore_net::{
    ClientWorldExt, ConnId, Incoming, NetClientPlugin, NetServerPlugin, ServerNet, SessionWorldExt,
};
use astrocore_prototypes::{LoadReport, Prototypes};

use super::*;

const PROTOTYPES: &str = "
- type: entity
  id: Player
- type: entity
  id: Console
  components:
  - type: UserInterface
    interfaces:
      enum.IdCardConsoleUiKey.Key:
        type: IdCardConsoleBoundUserInterface
";

const KEY: &str = "enum.IdCardConsoleUiKey.Key";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ConsoleState {
    name: String,
    access: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WriteToTargetId {
    name: String,
}

#[derive(Resource, Default)]
struct Log(Vec<String>);

fn setup(world: &mut World) {
    world.add_plugin(UiPlugin);
    world.register_ui_message::<WriteToTargetId>();
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str("t.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    assert_eq!(report.errors, []);
    world.insert_resource(prototypes);
}

#[test]
fn bound_ui_opens_updates_receives_messages_and_closes() {
    let mut server = World::new();
    server.add_plugin(NetServerPlugin);
    setup(&mut server);
    server.init_resource::<Log>();
    server.subscribe::<UserInterface, BoundUiOpened>(|world, _, event| {
        world
            .resource_mut::<Log>()
            .0
            .push(format!("opened {}", event.key));
    });
    server.subscribe::<UserInterface, BoundUiClosed>(|world, _, _| {
        world.resource_mut::<Log>().0.push("closed".into());
    });
    server.subscribe::<UserInterface, BoundUiMessage<WriteToTargetId>>(|world, entity, event| {
        world
            .resource_mut::<Log>()
            .0
            .push(format!("write {}", event.message.name));
        let state = ConsoleState {
            name: event.message.name.clone(),
            access: vec!["Captain".into()],
        };
        world.set_ui_state(entity, &event.key, &state);
    });
    let (map, _) = server.create_map(true);
    let grid = server.create_grid(map);
    server.set_tiles(
        grid,
        (0..16).map(|x| (TileCoord::new(x, 0, 0), Tile::new(1))),
    );
    let at = |x: f32| EntityCoordinates::new(grid, Vec2::new(x, 0.5));
    let player = server.spawn_attached("Player", at(1.5)).unwrap();
    let console = server.spawn_attached("Console", at(2.5)).unwrap();

    let mut client = World::new();
    client.add_plugin(NetClientPlugin);
    setup(&mut client);
    client.net_connect("Urist");
    let conn = ConnId(1);
    let mut attached = false;
    let mut pump = |server: &mut World, client: &mut World| {
        let outgoing = std::mem::take(&mut client.resource_mut::<ClientNet>().outgoing);
        server.resource_mut::<ServerNet>().incoming.extend(
            outgoing
                .into_iter()
                .map(|(_, bytes)| Incoming::Packet(conn, bytes)),
        );
        server.run_tick();
        if !attached && let Some(session) = server.resource::<ServerNet>().session(conn) {
            server.attach_player(session, player);
            attached = true;
        }
        let outgoing = std::mem::take(&mut server.resource_mut::<ServerNet>().outgoing);
        client
            .resource_mut::<ClientNet>()
            .incoming
            .extend(outgoing.into_iter().map(|packet| packet.bytes));
        client.run_tick();
    };
    for _ in 0..6 {
        pump(&mut server, &mut client);
    }

    // Сообщение в закрытое окно сервер не принимает.
    let console_net = *server.get::<NetEntity>(console).unwrap();
    let client_console = client
        .resource::<NetEntities>()
        .entity(console_net)
        .unwrap();
    client.send_ui_message(
        client_console,
        KEY,
        &WriteToTargetId {
            name: "Чужой".into(),
        },
    );
    pump(&mut server, &mut client);
    assert_eq!(server.resource::<Log>().0, Vec::<String>::new());

    // Состояние, заданное до открытия, приходит вместе с окном.
    let initial = ConsoleState {
        name: "Urist".into(),
        access: Vec::new(),
    };
    server.set_ui_state(console, KEY, &initial);
    assert!(!server.open_ui(console, "enum.Нет.Key", player));
    assert!(server.open_ui(console, KEY, player));
    assert!(!server.open_ui(console, KEY, player));
    pump(&mut server, &mut client);
    let open = client.resource::<OpenUis>().list.clone();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, "IdCardConsoleBoundUserInterface");
    assert_eq!(open[0].entity, client_console);
    assert_eq!(open[0].state::<ConsoleState>(), Some(initial));

    // Действие игрока доходит до сервера, новое состояние — до клиента.
    client.send_ui_message(
        client_console,
        KEY,
        &WriteToTargetId {
            name: "McHands".into(),
        },
    );
    pump(&mut server, &mut client);
    pump(&mut server, &mut client);
    assert_eq!(
        server.resource::<Log>().0,
        [format!("opened {KEY}"), "write McHands".into()]
    );
    let state = client.resource::<OpenUis>().list[0]
        .state::<ConsoleState>()
        .unwrap();
    assert_eq!((state.name.as_str(), state.access.len()), ("McHands", 1));

    // Игрок отошёл дальше двух тайлов: окно закрывается само.
    astrocore_map::TransformWorldExt::set_coordinates(&mut server, player, at(9.5), None).unwrap();
    pump(&mut server, &mut client);
    pump(&mut server, &mut client);
    assert!(client.resource::<OpenUis>().list.is_empty());
    assert!(!server.ui_open_for(console, KEY, player));

    // Игрок закрывает окно сам.
    astrocore_map::TransformWorldExt::set_coordinates(&mut server, player, at(1.5), None).unwrap();
    assert!(server.open_ui(console, KEY, player));
    pump(&mut server, &mut client);
    assert_eq!(client.resource::<OpenUis>().list.len(), 1);
    client.request_close_ui(client_console, KEY);
    pump(&mut server, &mut client);
    pump(&mut server, &mut client);
    assert!(client.resource::<OpenUis>().list.is_empty());
    assert_eq!(
        server
            .resource::<Log>()
            .0
            .iter()
            .filter(|line| *line == "closed")
            .count(),
        2
    );

    // Удаление сущности закрывает её окна.
    assert!(server.open_ui(console, KEY, player));
    pump(&mut server, &mut client);
    server.delete_entity(console);
    pump(&mut server, &mut client);
    assert!(client.resource::<OpenUis>().list.is_empty());
}
