use std::net::SocketAddr;
use std::time::{Duration, Instant};

use astrocore_lightyear::{LyClient, LyGameHost};
use astrocore_network::client::{ClientCore, ClientEvent};
use astrocore_network::server::{GameServer, ServerNetConfig};
use astrocore_network::testkit::{ByBuild, test_world};

const KEY: [u8; 32] = [7; 32];

fn game() -> GameServer {
    let cfg = ServerNetConfig {
        spawn: astrocore_math::glam::Vec3::new(2.5, 1.0, 2.5),
        ..ServerNetConfig::default()
    };
    GameServer::new(test_world(), cfg, Box::new(ByBuild))
}

struct Rig {
    host: LyGameHost,
    client: LyClient,
    core: ClientCore,
    events: Vec<ClientEvent>,
}

impl Rig {
    fn new() -> Self {
        let host = LyGameHost::start("127.0.0.1:0".parse().unwrap(), KEY, game(), 20);
        let client = LyClient::connect(host.local_addr(), 1, KEY, 60).unwrap();
        Self {
            host,
            client,
            core: ClientCore::new("a", test_world().id(), 0),
            events: Vec::new(),
        }
    }

    fn pump(&mut self) {
        self.host.step();
        self.client.step();
        for pkt in self.client.take_packets() {
            let evs = self.core.on_packet(&pkt).expect("пакет сервера корректен");
            if evs
                .iter()
                .any(|e| matches!(e, ClientEvent::ResyncRequested))
            {
                self.client.send(true, self.core.snapshot_request_packet());
            }
            self.events.extend(evs);
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    fn until(&mut self, secs: u64, mut done: impl FnMut(&mut Rig) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            self.pump();
            if done(self) {
                return true;
            }
        }
        false
    }
}

#[test]
fn client_joins_gets_the_world_and_moves_through_lightyear() {
    let mut r = Rig::new();
    assert!(
        r.until(20, |r| r.client.is_connected()),
        "соединение установлено"
    );
    assert!(r.client.send(true, r.core.hello_packet()));
    assert!(
        r.until(20, |r| r.core.is_ready()),
        "получен Welcome и полный снимок мира; событий: {}",
        r.events.len()
    );
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Joined { .. }))
    );
    assert_eq!(r.host.game().player_count(), 1);

    // Ввод идёт по ненадёжному каналу, подтверждение возвращается; позиция меняется.
    let start = r.core.position();
    for _ in 0..30 {
        if let Some(p) = r.core.input_packet(0, 1, std::f32::consts::PI) {
            r.client.send(false, p);
        }
        r.until(0, |_| false);
        for _ in 0..20 {
            r.pump();
        }
    }
    let end = r.core.position();
    assert!(
        end.distance(start) > 0.5,
        "персонаж сдвинулся: {start} → {end}"
    );
}

#[test]
fn a_client_with_the_wrong_key_never_connects() {
    let mut host = LyGameHost::start(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        KEY,
        game(),
        20,
    );
    let mut bad = LyClient::connect(host.local_addr(), 2, [9; 32], 60).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        host.step();
        bad.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!bad.is_connected());
    assert_eq!(host.game().player_count(), 0);
}

#[test]
fn disconnect_frees_the_player_slot_on_the_server() {
    let mut r = Rig::new();
    assert!(r.until(20, |r| r.client.is_connected()));
    r.client.send(true, r.core.hello_packet());
    assert!(r.until(20, |r| r.host.game().player_count() == 1));
    drop(r.client);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end && r.host.game().player_count() > 0 {
        r.host.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        r.host.game().player_count(),
        0,
        "таймаут соединения освободил слот"
    );
}

#[test]
fn packets_sent_before_the_handshake_finishes_are_queued_and_delivered() {
    let mut r = Rig::new();
    assert!(!r.client.is_connected());
    assert!(r.client.send(true, r.core.hello_packet()));
    assert!(
        r.until(20, |r| r.core.is_ready()),
        "Hello из очереди дошёл, мир получен"
    );
}
