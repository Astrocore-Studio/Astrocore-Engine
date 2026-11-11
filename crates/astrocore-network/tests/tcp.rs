//! Интеграционная проверка по настоящим сокетам: два клиента, один сервер на 127.0.0.1.

use std::time::{Duration, Instant};

use astrocore_core::ObjectId;
use astrocore_math::GridEdge;
use astrocore_network::client::{ClientCore, ClientEvent};
use astrocore_network::server::{GameServer, ServerNetConfig};
use astrocore_network::tcp::{TcpClient, TcpServer, write_frame};
use astrocore_network::testkit::{ByBuild, DOOR_ID, material, test_world};
use astrocore_world::{FloorId, Placement, StructureElement, StructureKind, WorldCommand};

struct Peer {
    net: TcpClient,
    core: ClientCore,
    events: Vec<ClientEvent>,
}

impl Peer {
    fn connect(addr: std::net::SocketAddr, build: &str, server_hash: u64) -> Self {
        let mut net = TcpClient::connect(addr).expect("подключение");
        let core = ClientCore::new(build, "testkit", server_hash);
        net.send(&core.hello_packet()).expect("Hello");
        Self {
            net,
            core,
            events: Vec::new(),
        }
    }

    fn pump(&mut self) {
        for packet in self.net.poll() {
            self.events.extend(
                self.core
                    .on_packet(&packet)
                    .expect("пакет сервера корректен"),
            );
        }
    }
}

fn run_until(
    server: &mut TcpServer,
    peers: &mut [&mut Peer],
    what: &str,
    mut done: impl FnMut(&[&mut Peer]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        server.step().expect("шаг сервера");
        for p in peers.iter_mut() {
            p.pump();
        }
        if done(peers) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("не дождались: {what}");
}

#[test]
fn two_clients_join_move_open_door_and_edit_over_tcp() {
    let world = test_world();
    let hash = world.content_hash();
    let cfg = ServerNetConfig {
        spawn: astrocore_math::glam::Vec3::new(2.5, 1.0, 2.5),
        ..ServerNetConfig::default()
    };
    let mut server = TcpServer::bind(
        "127.0.0.1:0",
        GameServer::new(world, cfg, Box::new(ByBuild)),
    )
    .expect("bind");
    let addr = server.local_addr().unwrap();

    let mut a = Peer::connect(addr, "edit-a", hash);
    let mut b = Peer::connect(addr, "b", 0); // у второго локальной карты нет
    run_until(
        &mut server,
        &mut [&mut a, &mut b],
        "оба клиента вошли",
        |p| p.iter().all(|x| x.core.is_ready()),
    );
    assert_eq!(server.core().player_count(), 2);
    assert_eq!(b.core.world().unwrap().content_hash(), hash);

    // Движение a: 10 тиков вперёд; предсказание совпадает с сервером.
    for _ in 0..10 {
        let p = a
            .core
            .input_packet(0, 1, -std::f32::consts::FRAC_PI_2)
            .unwrap();
        a.net.send(&p).unwrap();
        server.step().unwrap();
        a.pump();
        b.pump();
    }
    run_until(
        &mut server,
        &mut [&mut a, &mut b],
        "ввод подтверждён",
        |p| p[0].core.pending_inputs() == 0,
    );
    assert!(
        !a.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Corrected { .. }))
    );
    // b видит a.
    run_until(&mut server, &mut [&mut a, &mut b], "b видит a", |p| {
        !p[1].core.remote_entities().is_empty()
    });

    // Правка мира редактором доходит до обоих.
    let cmd = WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(123_456),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_x(5, 5)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.2,
    });
    let packet = a.core.edit_packet(cmd).unwrap();
    a.net.send(&packet).unwrap();
    run_until(
        &mut server,
        &mut [&mut a, &mut b],
        "правка у обоих",
        |p| {
            p.iter().all(|x| {
                x.core
                    .world()
                    .is_some_and(|w| w.element(ObjectId::from_u128(123_456)).is_some())
            })
        },
    );
    assert_eq!(
        a.core.world().unwrap().content_hash(),
        server.core().world().content_hash()
    );
    assert_eq!(
        b.core.world().unwrap().content_hash(),
        server.core().world().content_hash()
    );

    // Запертая серверной логикой дверь: взаимодействие отклоняется (игрок далеко от двери).
    let door = a.core.door_ids()[0];
    a.net.send(&a.core.interact_packet(door)).unwrap();
    run_until(
        &mut server,
        &mut [&mut a, &mut b],
        "ответ на взаимодействие",
        |p| {
            p[0].events
                .iter()
                .any(|e| matches!(e, ClientEvent::Rejected { .. }))
        },
    );
    assert!(!server.core().doors().is_open(DOOR_ID));
}

#[test]
fn hostile_connections_do_not_disturb_the_server() {
    let world = test_world();
    let mut server = TcpServer::bind(
        "127.0.0.1:0",
        GameServer::new(world, ServerNetConfig::default(), Box::new(ByBuild)),
    )
    .unwrap();
    let addr = server.local_addr().unwrap();

    // 1. Мусорный кадр до Hello.
    let mut junk = TcpClient::connect(addr).unwrap();
    junk.send(&[9, 9, 9, 9]).unwrap();
    // 2. Заявленная гигантская длина кадра.
    let mut raw = std::net::TcpStream::connect(addr).unwrap();
    use std::io::Write;
    raw.write_all(&u32::MAX.to_le_bytes()).unwrap();
    // 3. Пустой кадр и обрыв посреди кадра.
    let mut empty = std::net::TcpStream::connect(addr).unwrap();
    write_frame(&mut empty, &[]).unwrap();
    let mut cut = std::net::TcpStream::connect(addr).unwrap();
    cut.write_all(&[50, 0, 0, 0, 1, 2, 3]).unwrap();
    drop(cut);

    // Нормальный клиент после этого всё равно входит.
    let hash = server.core().world().content_hash();
    let mut good = Peer::connect(addr, "ok", hash);
    run_until(
        &mut server,
        &mut [&mut good],
        "добропорядочный клиент вошёл",
        |p| p[0].core.is_ready(),
    );
    assert!(server.core().player_count() == 1);
    // Мусорное соединение сервер закрыл.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !junk.is_closed() && Instant::now() < deadline {
        server.step().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(junk.is_closed());
}
