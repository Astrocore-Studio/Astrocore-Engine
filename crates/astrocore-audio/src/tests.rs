use astrocore_data::{DataFormat, from_node, parse_yaml, to_node};
use astrocore_ecs::{ComponentRegistry, GameWorldExt};
use astrocore_map::{EntityCoordinates, SpawnWorldExt, Tile, TileCoord};
use astrocore_net::{
    ClientNet, ClientWorldExt, ConnId, Incoming, NetClientPlugin, NetServerPlugin, ServerNet,
    SessionWorldExt,
};
use astrocore_prototypes::LoadReport;

use super::*;

const PROTOTYPES: &str = "
- type: soundCollection
  id: Sparks
  files:
  - /Audio/Effects/sparks1.ogg
  - /Audio/Effects/sparks2.ogg
- type: entity
  id: Player
- type: entity
  id: Generator
  components:
  - type: AmbientSound
    range: 5
    sound:
      path: /Audio/Ambience/Objects/engine_hum.ogg
";

fn setup(world: &mut World) {
    world.add_plugin(AudioPlugin);
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str("t.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    assert_eq!(report.errors, []);
    world.insert_resource(prototypes);
}

#[test]
fn sound_specifier_reads_ss14_forms() {
    let read = |text: &str| from_node::<SoundSpecifier>(&parse_yaml(text).unwrap());
    assert_eq!(
        read("/Audio/a.ogg").unwrap(),
        SoundSpecifier::path("/Audio/a.ogg")
    );
    assert_eq!(
        read("path: /Audio/a.ogg").unwrap(),
        SoundSpecifier::path("/Audio/a.ogg")
    );
    let sound = read("collection: Sparks\nparams:\n  volume: -4\n  variation: 0.1\n  loop: true\n")
        .unwrap();
    let params = sound.params();
    assert_eq!(
        (params.volume, params.variation, params.looped),
        (-4.0, Some(0.1), true)
    );
    assert_eq!(params.max_distance, DEFAULT_SOUND_RANGE);
    assert!(read("params: {volume: 1}").is_err());
    // Запись и чтение дают тот же звук: текстом и в сетевом формате.
    assert_eq!(
        from_node::<SoundSpecifier>(&to_node(&sound).unwrap()).unwrap(),
        sound
    );
    let bytes = postcard::to_allocvec(&sound).unwrap();
    assert_eq!(
        postcard::from_bytes::<SoundSpecifier>(&bytes).unwrap(),
        sound
    );
}

#[test]
fn gain_falls_with_distance_and_stops_at_max() {
    let params = AudioParams::default();
    assert_eq!(params.gain_at(0.0), 1.0);
    assert_eq!(params.gain_at(1.0), 1.0);
    assert!((params.gain_at(3.0) - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(params.gain_at(15.1), 0.0);
    let quiet = AudioParams {
        volume: -10.0,
        ..params
    };
    assert!((quiet.gain_at(1.0) - 0.1).abs() < 1e-6);
}

#[test]
fn collection_gives_one_of_its_files() {
    let mut world = World::new();
    world.init_game();
    setup(&mut world);
    let sound = SoundSpecifier::collection("Sparks").with_params(AudioParams {
        variation: Some(0.2),
        ..AudioParams::default()
    });
    for _ in 0..8 {
        let (path, params) = world.resolve_sound(&sound).unwrap();
        assert!(path == "Audio/Effects/sparks1.ogg" || path == "Audio/Effects/sparks2.ogg");
        assert!((0.8..=1.2).contains(&params.pitch));
    }
    assert_eq!(
        world.resolve_sound(&SoundSpecifier::collection("Нет")),
        None
    );
}

#[test]
fn only_players_in_range_hear_positional_sounds() {
    let mut server = World::new();
    server.add_plugin(NetServerPlugin);
    setup(&mut server);
    let (map, _) = server.create_map(true);
    let grid = server.create_grid(map);
    server.set_tiles(
        grid,
        (0..64).map(|x| (TileCoord::new(x, 0, 0), Tile::new(1))),
    );
    let at = |x: f32| EntityCoordinates::new(grid, Vec2::new(x, 0.5));
    let near = server.spawn_attached("Player", at(2.5)).unwrap();
    let far = server.spawn_attached("Player", at(40.5)).unwrap();
    let generator = server.spawn_attached("Generator", at(4.5)).unwrap();

    let mut clients: Vec<World> = (0..2)
        .map(|_| {
            let mut client = World::new();
            client.add_plugin(NetClientPlugin);
            setup(&mut client);
            client.net_connect("Urist");
            client
        })
        .collect();
    let players = [near, far];
    let mut attached = [false, false];
    let mut pump = |server: &mut World, clients: &mut Vec<World>| {
        for (index, client) in clients.iter_mut().enumerate() {
            let conn = ConnId(index as u64 + 1);
            let outgoing = std::mem::take(&mut client.resource_mut::<ClientNet>().outgoing);
            server.resource_mut::<ServerNet>().incoming.extend(
                outgoing
                    .into_iter()
                    .map(|(_, bytes)| Incoming::Packet(conn, bytes)),
            );
        }
        server.run_tick();
        for index in 0..2 {
            let conn = ConnId(index as u64 + 1);
            if !attached[index]
                && let Some(session) = server.resource::<ServerNet>().session(conn)
            {
                server.attach_player(session, players[index]);
                attached[index] = true;
            }
        }
        let outgoing = std::mem::take(&mut server.resource_mut::<ServerNet>().outgoing);
        for packet in outgoing {
            let client = &mut clients[packet.conn.0 as usize - 1];
            client
                .resource_mut::<ClientNet>()
                .incoming
                .push(packet.bytes);
        }
        for client in clients.iter_mut() {
            client.run_tick();
        }
    };
    for _ in 0..6 {
        pump(&mut server, &mut clients);
    }

    let spark = SoundSpecifier::collection("Sparks");
    server.play_entity(&spark, generator, None);
    server.play_global(
        &SoundSpecifier::path("/Audio/Announcements/attention.ogg"),
        NetTarget::All,
    );
    // Игрок, который проиграл звук сам, второй раз его не получает.
    server.play_entity(&spark, generator, Some(near));
    pump(&mut server, &mut clients);

    let heard = |client: &World| -> Vec<String> {
        client
            .resource::<SoundQueue>()
            .0
            .iter()
            .map(|s| s.path.clone())
            .collect()
    };
    let near_heard = heard(&clients[0]);
    assert_eq!(near_heard.len(), 2, "{near_heard:?}");
    assert!(near_heard[0].starts_with("Audio/Effects/sparks"));
    assert!(matches!(
        clients[0].resource::<SoundQueue>().0[0].source,
        SoundSource::Entity(_)
    ));
    assert_eq!(heard(&clients[1]), ["Audio/Announcements/attention.ogg"]);

    // Фоновый звук приходит клиенту компонентом прототипа.
    let ambient: Vec<&AmbientSound> = clients[0]
        .query::<&AmbientSound>()
        .iter(&clients[0])
        .collect();
    assert_eq!(ambient.len(), 1);
    assert_eq!(
        (ambient[0].range, ambient[0].volume, ambient[0].enabled),
        (5.0, -10.0, true)
    );
}
