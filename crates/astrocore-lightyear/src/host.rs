use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use astrocore_network::server::{ConnId, GameServer, Out, encode_out};
use astrocore_protocol::ServerMessage;

use crate::{ConnKey, LyServer};

/// Игровой сервер, который слушает соединения Lightyear: связывает приём пакетов, тики
/// `GameServer` и отправку ответов. Хозяин вызывает `step` в цикле.
pub struct LyGameHost {
    ly: LyServer,
    game: GameServer,
    known: BTreeSet<ConnKey>,
    tick_period: Duration,
    last_tick: Instant,
}

fn conn(k: ConnKey) -> ConnId {
    ConnId(u64::from(k.0))
}

impl LyGameHost {
    pub fn start(addr: SocketAddr, key: [u8; 32], game: GameServer, tick_hz: u8) -> Self {
        Self {
            ly: LyServer::start(addr, key, 60),
            game,
            known: BTreeSet::new(),
            tick_period: Duration::from_secs_f64(1.0 / f64::from(tick_hz.max(1))),
            last_tick: Instant::now(),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.ly.local_addr()
    }

    pub fn game(&self) -> &GameServer {
        &self.game
    }

    pub fn game_mut(&mut self) -> &mut GameServer {
        &mut self.game
    }

    fn route(&mut self, outs: Vec<Out>) {
        let mut queue = outs;
        while let Some(out) = queue.pop() {
            match out {
                Out::Send { to, msg } => {
                    if let Some(bytes) = encode_out(&msg) {
                        let reliable = !matches!(
                            msg,
                            ServerMessage::EntityState { .. } | ServerMessage::InputAck { .. }
                        );
                        self.ly.send(ConnKey(to.0 as u32), reliable, bytes);
                    }
                }
                Out::Disconnect(c) => queue.extend(self.game.disconnect(c)),
            }
        }
    }

    /// Один шаг: сеть Lightyear, пакеты в игру, тик по расписанию, ответы в сеть.
    pub fn step(&mut self) {
        self.ly.step();
        let live: BTreeSet<ConnKey> = self.ly.connections().into_iter().collect();
        for c in live.difference(&self.known).copied().collect::<Vec<_>>() {
            let outs = self.game.connect(conn(c));
            self.route(outs);
        }
        for c in self.known.difference(&live).copied().collect::<Vec<_>>() {
            let outs = self.game.disconnect(conn(c));
            self.route(outs);
        }
        self.known = live;
        for (c, bytes) in self.ly.take_packets() {
            let outs = self.game.on_packet(conn(c), &bytes);
            self.route(outs);
        }
        if self.last_tick.elapsed() >= self.tick_period {
            self.last_tick = Instant::now();
            let outs = self.game.tick();
            self.route(outs);
        }
    }
}
