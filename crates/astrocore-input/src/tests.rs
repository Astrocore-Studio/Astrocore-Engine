use astrocore_data::parse_yaml;
use astrocore_ecs::GameWorldExt;
use astrocore_map::{EntityCoordinates, MapWorldExt, SpawnWorldExt, Tile, TileCoord};
use astrocore_math::Vec2;
use astrocore_net::{
    ClientNet, ClientWorldExt, ConnId, Incoming, NetClientPlugin, NetEntities, NetEntity,
    NetServerPlugin, ServerNet, SessionWorldExt,
};
use astrocore_prototypes::Prototypes;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use super::*;

const BINDS: &str = "
version: 1
binds:
- {function: MoveUp, type: State, key: W}
- {function: Walk, type: State, key: Shift}
- {function: Use, type: State, key: MouseLeft, canFocus: true}
- {function: UIClick, type: State, key: MouseLeft, canFocus: true, priority: 5}
- {function: AltUse, type: State, key: MouseLeft, mod1: Alt}
- {function: ExamineEntity, type: State, key: MouseLeft, mod1: Shift, allowSubCombs: true}
- {function: ShowDebugMonitors, type: Toggle, key: F3}
- {function: EditorPlace, type: State, key: MouseLeft}
";

fn bindings() -> (KeyBindings, InputContexts) {
    let mut bindings = KeyBindings::default();
    bindings.load_defaults(&parse_yaml(BINDS).unwrap()).unwrap();
    let mut contexts = engine_contexts();
    contexts.add_functions("human", ["AltUse", "ExamineEntity"]);
    contexts.set_active("human");
    (bindings, contexts)
}

fn names(changes: &[FunctionChange]) -> Vec<(&str, BoundKeyState)> {
    changes
        .iter()
        .map(|c| (c.function.as_str(), c.state))
        .collect()
}

use BoundKeyState::{Down, Up};

#[test]
fn keys_fire_functions_of_active_context_by_priority() {
    let (mut bindings, contexts) = bindings();
    assert_eq!(
        names(&bindings.key_down("W", &contexts)),
        [("MoveUp", Down)]
    );
    // Повтор нажатия удерживаемой клавиши ничего не меняет.
    assert_eq!(bindings.key_down("W", &contexts), []);
    assert_eq!(names(&bindings.key_up("W")), [("MoveUp", Up)]);
    // Одна клавиша — две функции: первой идёт функция с большим приоритетом. Функция
    // редактора вне активного контекста молчит.
    assert_eq!(
        names(&bindings.key_down("MouseLeft", &contexts)),
        [("UIClick", Down), ("Use", Down)]
    );
    assert_eq!(
        names(&bindings.key_up("MouseLeft")),
        [("UIClick", Up), ("Use", Up)]
    );
    // В общем контексте движения нет.
    let mut common = contexts.clone();
    common.set_active(COMMON_CONTEXT);
    assert_eq!(bindings.key_down("W", &common), []);
    assert!(!common.set_active("нет такого"));
}

#[test]
fn combos_win_over_their_parts() {
    let (mut bindings, contexts) = bindings();
    // Alt + клик: срабатывает только сочетание.
    assert_eq!(bindings.key_down("Alt", &contexts), []);
    assert_eq!(
        names(&bindings.key_down("MouseLeft", &contexts)),
        [("AltUse", Down)]
    );
    assert_eq!(names(&bindings.key_up("MouseLeft")), [("AltUse", Up)]);
    bindings.key_up("Alt");
    // Shift + клик разрешает части сочетания: срабатывают и осмотр, и обычный клик.
    assert_eq!(
        names(&bindings.key_down("Shift", &contexts)),
        [("Walk", Down)]
    );
    assert_eq!(
        names(&bindings.key_down("MouseLeft", &contexts)),
        [("ExamineEntity", Down), ("UIClick", Down), ("Use", Down)]
    );
    // Отпускание модификатора отпускает сочетание и сам модификатор.
    assert_eq!(
        names(&bindings.key_up("Shift")),
        [("ExamineEntity", Up), ("Walk", Up)]
    );
    assert_eq!(
        names(&bindings.release_all()),
        [("UIClick", Up), ("Use", Up)]
    );
}

#[test]
fn toggle_flips_on_each_press() {
    let (mut bindings, contexts) = bindings();
    assert_eq!(
        names(&bindings.key_down("F3", &contexts)),
        [("ShowDebugMonitors", Down)]
    );
    assert_eq!(bindings.key_up("F3"), []);
    assert_eq!(
        names(&bindings.key_down("F3", &contexts)),
        [("ShowDebugMonitors", Up)]
    );
}

#[test]
fn player_rebinds_survive_save_and_load() {
    let (mut bindings, contexts) = bindings();
    let arrow = KeyBind {
        function: "MoveUp".into(),
        kind: BindKind::State,
        key: "Up".into(),
        mod1: None,
        mod2: None,
        mod3: None,
        can_focus: false,
        can_repeat: false,
        allow_sub_combs: false,
        priority: 0,
    };
    bindings.rebind("MoveUp", vec![arrow.clone()]);
    bindings.rebind("Walk", Vec::new());
    assert_eq!(bindings.key_down("W", &contexts), []);
    assert_eq!(bindings.key_down("Shift", &contexts), []);
    assert_eq!(
        names(&bindings.key_down("Up", &contexts)),
        [("MoveUp", Down)]
    );

    let saved = bindings.user_yaml();
    let (mut loaded, _) = self::bindings();
    loaded.load_user(&parse_yaml(&saved).unwrap()).unwrap();
    assert_eq!(loaded.binds_of("MoveUp").collect::<Vec<_>>(), [&arrow]);
    assert_eq!(loaded.binds_of("Walk").count(), 0);
    assert_eq!(loaded.binds_of("Use").count(), 1);
    // Сохраняются только изменения игрока.
    assert!(!saved.contains("UIClick"), "{saved}");

    loaded.reset("MoveUp");
    assert_eq!(
        loaded.binds_of("MoveUp").next().map(|b| b.key.as_str()),
        Some("W")
    );
}

#[derive(Resource, Default)]
struct Seen(Vec<InputArgs>);

#[derive(Resource, Default)]
struct UseSeen(usize);

#[test]
fn input_command_reaches_server_handlers() {
    let prototypes = "- type: entity\n  id: Player\n- type: entity\n  id: Lever\n  components:\n  - type: Clickable\n";
    // `handled`: обработчик клиента забирает команду себе — на сервер она не идёт.
    let setup = |world: &mut World, handled: bool| {
        world.add_plugin(InputPlugin);
        let mut loaded = Prototypes::default();
        let mut report = astrocore_prototypes::LoadReport::default();
        loaded.load_str(
            "t.yml",
            astrocore_data::DataFormat::Yaml,
            prototypes,
            &mut report,
        );
        loaded.resolve(
            world.resource::<astrocore_ecs::ComponentRegistry>(),
            &mut report,
        );
        assert_eq!(report.errors, []);
        world.insert_resource(loaded);
        world.init_resource::<Seen>();
        world.bind_input("ActivateItemInWorld", |world, args| {
            world.resource_mut::<Seen>().0.push(args.clone());
            false
        });
        // Отложенный щелчок (как у перетаскивания): забирается, кроме повтора.
        world.bind_input("Use", move |_, args| handled && !args.replay);
    };
    let mut server = World::new();
    server.add_plugin(NetServerPlugin);
    setup(&mut server, false);
    server.init_resource::<UseSeen>();
    server.bind_input("Use", |world, _| {
        world.resource_mut::<UseSeen>().0 += 1;
        false
    });
    let (map, map_id) = server.create_map(true);
    let grid = server.create_grid(map);
    server.set_tiles(
        grid,
        (0..8).map(|x| (TileCoord::new(x, 0, 0), Tile::new(1))),
    );
    let at = |x: f32| EntityCoordinates::new(grid, Vec2::new(x, 0.5));
    let player = server.spawn_attached("Player", at(1.5)).unwrap();
    let lever = server.spawn_attached("Lever", at(3.5)).unwrap();

    let mut client = World::new();
    client.add_plugin(NetClientPlugin);
    setup(&mut client, true);
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
    let lever_net = *server.get::<NetEntity>(lever).unwrap();
    let client_lever = client.resource::<NetEntities>().entity(lever_net).unwrap();
    assert!(client.get::<Clickable>(client_lever).is_some());

    // Клиент: свой обработчик срабатывает сразу, необработанная команда уходит серверу.
    assert!(!client.send_input(
        "ActivateItemInWorld",
        Down,
        Some(Vec2::new(3.5, 0.5)),
        Some(client_lever)
    ));
    assert!(!client.send_input("Unbound", Down, None, None));
    // Обработанная клиентом — нет; её повтор — да.
    assert!(client.send_input("Use", Down, None, Some(client_lever)));
    assert!(!client.replay_input("Use", Down, None, Some(client_lever)));
    assert_eq!(client.resource::<Seen>().0.len(), 1);
    assert_eq!(client.resource::<Seen>().0[0].target, Some(client_lever));
    pump(&mut server, &mut client);
    let seen = &server.resource::<Seen>().0;
    assert_eq!(seen.len(), 1);
    assert_eq!(server.resource::<UseSeen>().0, 1);
    assert_eq!(seen[0].player, Some(player));
    assert_eq!(seen[0].target, Some(lever));
    assert!(seen[0].session.is_some());
    let coordinates = seen[0].coordinates.unwrap();
    assert_eq!(
        (coordinates.map, coordinates.position),
        (map_id, Vec2::new(3.5, 0.5))
    );
}
