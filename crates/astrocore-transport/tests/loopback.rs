//! Сквозная проверка: сервер и клиент на ядре сущностей через настоящий UDP на localhost.

use std::time::{Duration, Instant};

use astrocore_ecs::bevy_ecs::world::World;
use astrocore_ecs::{GameWorldExt, LifecycleWorldExt};
use astrocore_map::{
    EntityCoordinates, MapGrid, MapWorldExt, Tile, TileCoord, Transform, TransformWorldExt,
};
use astrocore_math::Vec2;
use astrocore_net::{
    ClientNet, ClientStatus, ClientWorldExt, NetClientPlugin, NetEntities, NetEntity,
    NetServerPlugin, ServerNet, SessionWorldExt,
};
use astrocore_transport::{ClientLink, ServerLink};

const KEY: [u8; 32] = [7; 32];
const TICK_HZ: u32 = 60;

#[test]
fn client_joins_over_udp_and_follows_server() {
    let mut server = World::new();
    server.add_plugin(NetServerPlugin);
    let (map, _) = server.create_map(true);
    let grid = server.create_grid(map);
    server.set_tiles(
        grid,
        (0..8).flat_map(|x| (0..8).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1)))),
    );
    let player = server.spawn_entity();
    server
        .set_coordinates(
            player,
            EntityCoordinates::new(grid, Vec2::new(1.5, 1.5)),
            None,
        )
        .unwrap();
    server.initialize_and_start(player, true);
    let mut server_link = ServerLink::start("127.0.0.1:0".parse().unwrap(), KEY, TICK_HZ);

    let mut client = World::new();
    client.add_plugin(NetClientPlugin);
    let mut client_link = ClientLink::connect(server_link.local_addr(), 42, KEY, TICK_HZ).unwrap();
    client.net_connect("Urist");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut attached = false;
    let mut moved = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "клиент не догнал сервер за 20 секунд"
        );
        server_link.receive(&mut server);
        let session = server.resource::<ServerNet>().sessions().next();
        if !attached && let Some(session) = session {
            server.attach_player(session, player);
            attached = true;
        }
        server.run_tick();
        server_link.send(&mut server);

        client_link.receive(&mut client);
        client.run_tick();
        client_link.send(&mut client);

        let Some(local) = client.resource::<ClientNet>().attached else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        assert_eq!(
            client.resource::<ClientNet>().status,
            ClientStatus::Connected
        );
        if !moved {
            // Клиент получил мир: грид с тайлами и свою сущность.
            let net = *server.get::<NetEntity>(grid).unwrap();
            let grid_local = client.resource::<NetEntities>().entity(net).unwrap();
            assert_eq!(client.get::<MapGrid>(grid_local).unwrap().tile_count(), 64);
            server
                .set_local_position(player, Vec2::new(5.5, 6.5))
                .unwrap();
            moved = true;
        }
        if client.get::<Transform>(local).unwrap().local_position() == Vec2::new(5.5, 6.5) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(client_link.is_connected());
}
