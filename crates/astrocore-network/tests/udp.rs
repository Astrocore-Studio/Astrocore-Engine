//! UDP-транспорт: шифрование, потери, повторы, подмена, обрыв, настоящие сокеты.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use astrocore_core::ObjectId;
use astrocore_math::GridEdge;
use astrocore_network::client::{ClientCore, ClientEvent};
use astrocore_network::secure::StaticKeypair;
use astrocore_network::server::{GameServer, ServerNetConfig};
use astrocore_network::testkit::{ByBuild, SimNet, SimSocket, material, test_world};
use astrocore_network::udp::{ClientError, SystemSocket, UdpClient, UdpServer};
use astrocore_world::{FloorId, Placement, StructureElement, StructureKind, WorldCommand};

fn addr(port: u16) -> SocketAddr {
    format!("10.0.0.{}:{port}", port % 200 + 1).parse().unwrap()
}

fn cfg() -> ServerNetConfig {
    ServerNetConfig {
        spawn: astrocore_math::glam::Vec3::new(2.5, 1.0, 2.5),
        ..ServerNetConfig::default()
    }
}

fn server_on(net: &SimNet, keys: &StaticKeypair) -> UdpServer<SimSocket> {
    UdpServer::new(
        net.socket(addr(7000)),
        keys.clone(),
        GameServer::new(test_world(), cfg(), Box::new(ByBuild)),
        20,
    )
}

struct Peer<S: astrocore_network::udp::Datagram> {
    udp: UdpClient<S>,
    core: ClientCore,
    events: Vec<ClientEvent>,
    hello_sent: bool,
}

impl<S: astrocore_network::udp::Datagram> Peer<S> {
    fn new(udp: UdpClient<S>, build: &str, world_hash: u64) -> Self {
        Self {
            udp,
            core: ClientCore::new(build, "testkit", world_hash),
            events: Vec::new(),
            hello_sent: false,
        }
    }

    fn step(&mut self, now: u64) {
        self.udp.step(now).unwrap();
        if self.udp.is_established() && !self.hello_sent {
            self.udp.send(true, &self.core.hello_packet()).unwrap();
            self.hello_sent = true;
        }
        for p in self.udp.take_packets() {
            for ev in self.core.on_packet(&p).expect("пакет сервера корректен")
            {
                if matches!(ev, ClientEvent::ResyncRequested) {
                    self.udp
                        .send(true, &self.core.snapshot_request_packet())
                        .unwrap();
                }
                self.events.push(ev);
            }
        }
    }

    fn input(&mut self, mx: i8, mz: i8, yaw: f32) {
        if let Some(p) = self.core.input_packet(mx, mz, yaw) {
            self.udp.send(false, &p).unwrap();
        }
    }
}

fn wall(id: u128, x: i32, z: i32) -> WorldCommand {
    WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(id),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_x(x, z)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.2,
    })
}

fn run_sim(
    net: &SimNet,
    server: &mut UdpServer<SimSocket>,
    peers: &mut [&mut Peer<SimSocket>],
    from: u64,
    to: u64,
    mut each: impl FnMut(u64, &mut [&mut Peer<SimSocket>]),
) -> u64 {
    let mut now = from;
    while now < to {
        net.set_time(now);
        server.step(now).unwrap();
        for p in peers.iter_mut() {
            p.step(now);
        }
        each(now, peers);
        now += 10;
    }
    now
}

#[test]
fn clients_join_move_and_edit_over_lossy_encrypted_udp() {
    for seed in [1u64, 2, 3] {
        let net = SimNet::new(seed, 20, 10, (5, 40));
        let keys = StaticKeypair::generate().unwrap();
        let mut server = server_on(&net, &keys);
        let hash = server.core().world().content_hash();
        let mut a = Peer::new(
            UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
            "edit-a",
            hash,
        );
        let mut b = Peer::new(
            UdpClient::connect(net.socket(addr(5002)), addr(7000), keys.public()).unwrap(),
            "b",
            0,
        );

        // Вход под потерями: снимок мира идёт надёжным каналом.
        let mut now = run_sim(
            &net,
            &mut server,
            &mut [&mut a, &mut b],
            0,
            30_000,
            |_, _| {},
        );
        assert!(
            a.core.is_ready() && b.core.is_ready(),
            "seed {seed}: оба вошли"
        );
        assert_eq!(server.core().player_count(), 2);
        assert_eq!(b.core.world().unwrap().content_hash(), hash);

        // Движение a: ввод по ненадёжному каналу, затем «холостые» команды, чтобы дошло подтверждение.
        let mut sent = 0;
        let step_end = now + 6_000;
        let mut next_input = now;
        while now < step_end {
            net.set_time(now);
            server.step(now).unwrap();
            a.step(now);
            b.step(now);
            if now >= next_input {
                sent += 1;
                if sent <= 40 {
                    a.input(0, 1, -std::f32::consts::FRAC_PI_2);
                } else {
                    a.input(0, 0, 0.0);
                }
                next_input += 50;
            }
            now += 10;
        }
        // Какой из двух соединений — игрок a, заранее неизвестно (порядок рукопожатий зависит от потерь):
        // двигался только он.
        let server_pos = [1u64, 2]
            .iter()
            .filter_map(|c| {
                server
                    .core()
                    .player_position(astrocore_network::server::ConnId(*c))
            })
            .max_by(|p, q| p.x.total_cmp(&q.x))
            .unwrap();
        assert!(
            (a.core.position() - server_pos).length() < 0.05,
            "seed {seed}: согласованы {} vs {}",
            a.core.position(),
            server_pos
        );
        assert!(
            server_pos.x > 3.5,
            "игрок прошёл вперёд (часть ввода потерялась — это нормально): {server_pos}"
        );

        // Правка редактором (надёжный канал) доходит до обоих и до сервера.
        let edit = a.core.edit_packet(wall(900_000, 3, 3)).unwrap();
        a.udp.send(true, &edit).unwrap();
        run_sim(
            &net,
            &mut server,
            &mut [&mut a, &mut b],
            now,
            now + 10_000,
            |_, _| {},
        );
        for p in [&a, &b] {
            assert!(
                p.core
                    .world()
                    .unwrap()
                    .element(ObjectId::from_u128(900_000))
                    .is_some(),
                "seed {seed}"
            );
            assert_eq!(
                p.core.world().unwrap().content_hash(),
                server.core().world().content_hash()
            );
        }
        assert!(server.stats().dropped_bad == 0 || net.wire().len() > 100);
    }
}

#[test]
fn nothing_readable_is_on_the_wire_and_the_handshake_pins_the_server_key() {
    let net = SimNet::new(11, 0, 0, (1, 3));
    let keys = StaticKeypair::generate().unwrap();
    let mut server = server_on(&net, &keys);
    let hash = server.core().world().content_hash();
    let mut a = Peer::new(
        UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
        "secret-build-name",
        hash,
    );
    run_sim(&net, &mut server, &mut [&mut a], 0, 3000, |_, _| {});
    assert!(a.core.is_ready());
    let wire = net.wire();
    assert!(wire.len() > 10);
    for needle in [
        &b"testkit"[..],
        b"secret-build-name",
        b"test:concrete",
        b"ground",
    ] {
        assert!(
            !wire
                .iter()
                .any(|d| d.windows(needle.len()).any(|w| w == needle)),
            "открытый текст на линии: {:?}",
            String::from_utf8_lossy(needle)
        );
    }

    // Клиент с неверным закреплённым ключом (подмена сервера) не соединяется и ничего не отправляет игре.
    let net2 = SimNet::new(12, 0, 0, (1, 3));
    let mut server2 = server_on(&net2, &keys);
    let wrong = StaticKeypair::generate().unwrap();
    let mut evil = Peer::new(
        UdpClient::connect(net2.socket(addr(5003)), addr(7000), wrong.public()).unwrap(),
        "x",
        0,
    );
    run_sim(&net2, &mut server2, &mut [&mut evil], 0, 3000, |_, _| {});
    assert!(
        matches!(evil.udp.error(), Some(ClientError::Secure(_))),
        "{:?}",
        evil.udp.error()
    );
    assert!(!evil.core.is_ready());
}

#[test]
fn replays_forgeries_and_garbage_do_not_disturb_established_sessions() {
    let net = SimNet::new(21, 0, 0, (1, 3));
    let keys = StaticKeypair::generate().unwrap();
    let mut server = server_on(&net, &keys);
    let hash = server.core().world().content_hash();
    let mut a = Peer::new(
        UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
        "a",
        hash,
    );
    let now = run_sim(&net, &mut server, &mut [&mut a], 0, 3000, |_, _| {});
    assert!(a.core.is_ready());
    let before = server.stats();
    let hash_before = server.core().world().content_hash();

    // Атакующий воспроизводит перехваченные клиентские записи (с поддельным адресом отправителя).
    let captured: Vec<Vec<u8>> = net
        .wire()
        .into_iter()
        .filter(|d| d[0] == 0xA3)
        .take(20)
        .collect();
    assert!(!captured.is_empty());
    for d in &captured {
        net.inject(addr(5001), addr(7000), d.clone());
    }
    // Подделка: изменённая запись, мусор, чужой источник, рукопожатие без дополнения.
    let mut forged = captured[0].clone();
    let last = forged.len() - 1;
    forged[last] ^= 0xFF;
    net.inject(addr(5001), addr(7000), forged);
    net.inject(addr(5001), addr(7000), vec![0xA3, 1, 2, 3]);
    net.inject(addr(6666), addr(7000), vec![0xA3; 200]);
    net.inject(addr(6666), addr(7000), vec![0xA1; 40]);
    net.inject(addr(6666), addr(7000), (0..255u8).collect());
    run_sim(&net, &mut server, &mut [&mut a], now, now + 2000, |_, _| {});
    let after = server.stats();
    assert!(
        after.dropped_bad >= before.dropped_bad + captured.len() as u64,
        "повторы отброшены: {before:?} → {after:?}"
    );
    assert_eq!(server.session_count(), 1);
    assert_eq!(server.core().player_count(), 1);
    assert_eq!(server.core().world().content_hash(), hash_before);
    assert!(a.core.is_ready() && a.udp.error().is_none());
}

#[test]
fn heavy_loss_still_completes_the_handshake_and_dead_links_time_out() {
    let net = SimNet::new(31, 50, 0, (5, 30));
    let keys = StaticKeypair::generate().unwrap();
    let mut server = server_on(&net, &keys);
    let hash = server.core().world().content_hash();
    let mut a = Peer::new(
        UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
        "a",
        hash,
    );
    let now = run_sim(&net, &mut server, &mut [&mut a], 0, 40_000, |_, _| {});
    assert!(
        a.core.is_ready(),
        "50% потерь: рукопожатие и снимок всё равно дошли"
    );

    // Обрыв связи: канал обнаруживает мёртвого собеседника, сервер освобождает игрока.
    net.set_partitioned(true);
    let mut client_err = None;
    let mut t = now;
    while t < now + 30_000 {
        net.set_time(t);
        server.step(t).unwrap();
        let _ = a.udp.step(t);
        if let Some(e) = a.udp.error() {
            client_err = Some(e.clone());
            break;
        }
        t += 10;
    }
    assert!(
        matches!(client_err, Some(ClientError::Channel(_))),
        "{client_err:?}"
    );
    // Сервер тоже перестал ждать.
    while t < now + 40_000 && server.session_count() > 0 {
        net.set_time(t);
        server.step(t).unwrap();
        t += 10;
    }
    assert_eq!(server.session_count(), 0);
    assert_eq!(server.core().player_count(), 0);

    // Сервера нет вообще: рукопожатие завершается ошибкой по таймауту.
    let net3 = SimNet::new(33, 0, 0, (1, 2));
    let mut lone = UdpClient::connect(net3.socket(addr(5004)), addr(7000), keys.public()).unwrap();
    for t in (0..20_000).step_by(10) {
        net3.set_time(t);
        lone.step(t).unwrap();
    }
    assert_eq!(lone.error(), Some(&ClientError::HandshakeTimeout));
}

#[test]
fn a_restarted_client_on_the_same_address_replaces_its_old_session() {
    let net = SimNet::new(41, 0, 0, (1, 3));
    let keys = StaticKeypair::generate().unwrap();
    let mut server = server_on(&net, &keys);
    let hash = server.core().world().content_hash();
    let mut a = Peer::new(
        UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
        "a",
        hash,
    );
    let now = run_sim(&net, &mut server, &mut [&mut a], 0, 2000, |_, _| {});
    assert_eq!(server.core().player_count(), 1);
    drop(a);
    let mut a2 = Peer::new(
        UdpClient::connect(net.socket(addr(5001)), addr(7000), keys.public()).unwrap(),
        "a",
        hash,
    );
    run_sim(
        &net,
        &mut server,
        &mut [&mut a2],
        now,
        now + 3000,
        |_, _| {},
    );
    assert!(a2.core.is_ready());
    assert_eq!(server.session_count(), 1, "старая сессия заменена");
    assert_eq!(server.core().player_count(), 1);
}

#[test]
fn real_udp_sockets_work_end_to_end() {
    let keys = StaticKeypair::generate().unwrap();
    let sock = SystemSocket::bind("127.0.0.1:0").unwrap();
    let server_addr = sock.local_addr().unwrap();
    let world = test_world();
    let hash = world.content_hash();
    let mut server = UdpServer::new(
        sock,
        keys.clone(),
        GameServer::new(world, cfg(), Box::new(ByBuild)),
        20,
    );

    let mk = |build: &str, h: u64| {
        let s = SystemSocket::bind("127.0.0.1:0").unwrap();
        Peer::new(
            UdpClient::connect(s, server_addr, keys.public()).unwrap(),
            build,
            h,
        )
    };
    let mut a = mk("edit-a", hash);
    let mut b = mk("b", 0);
    let start = Instant::now();
    let now_ms = |s: &Instant| s.elapsed().as_millis() as u64;
    let mut sent_edit = false;
    loop {
        let now = now_ms(&start);
        server.step(now).unwrap();
        a.step(now);
        b.step(now);
        if a.core.is_ready() && b.core.is_ready() && !sent_edit {
            a.udp
                .send(true, &a.core.edit_packet(wall(777_000, 5, 5)).unwrap())
                .unwrap();
            sent_edit = true;
        }
        let done = sent_edit
            && [&a, &b].iter().all(|p| {
                p.core
                    .world()
                    .is_some_and(|w| w.element(ObjectId::from_u128(777_000)).is_some())
            });
        if done {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "не дождались");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        a.core.world().unwrap().content_hash(),
        server.core().world().content_hash()
    );
    assert_eq!(
        b.core.world().unwrap().content_hash(),
        server.core().world().content_hash()
    );
    assert_eq!(server.session_count(), 2);
}
