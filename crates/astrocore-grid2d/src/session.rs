//! Серверная симуляция с текущим снимком состояния и неизменяемой картой на время сессии.
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

use astrocore_lightyear::{ConnKey, LyClient, LyServer};
use astrocore_protocol::MAX_PACKET_BYTES;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const PROTOCOL_ID: u64 = 0x4153_5452_3244_0005;
const MOVEMENT_TIMEOUT_TICKS: u64 = 5;
pub const TICK_HZ: u32 = 20;
pub const TICK_PERIOD: Duration = Duration::from_millis(1000 / TICK_HZ as u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(u64);

#[derive(Debug)]
pub struct Input<A> {
    pub movement: [i8; 2],
    pub action: Option<A>,
}

pub trait Simulation {
    type Layout: Serialize;
    type State: Serialize;
    type Action: DeserializeOwned;

    fn layout(&self) -> &Self::Layout;
    fn state(&self) -> &Self::State;
    fn joined(&mut self, player: PlayerId);
    fn left(&mut self, player: PlayerId);
    fn tick(&mut self, inputs: &BTreeMap<PlayerId, Input<Self::Action>>);
}

#[derive(Serialize, Deserialize)]
enum ClientMessage<A> {
    Move([i8; 2]),
    Action(A),
}

#[derive(Serialize, Deserialize)]
enum ServerMessage<Layout, State> {
    Welcome {
        player: PlayerId,
        tick: u64,
        layout: Layout,
        state: State,
    },
    State {
        tick: u64,
        state: State,
    },
}

fn packet(message: &impl Serialize) -> Result<Vec<u8>, String> {
    let bytes = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PACKET_BYTES {
        return Err(format!("пакет 2D превышает лимит {MAX_PACKET_BYTES} байт"));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_PACKET_BYTES {
        return None;
    }
    let (message, rest) = postcard::take_from_bytes(bytes).ok()?;
    rest.is_empty().then_some(message)
}

struct Peer<A> {
    player: PlayerId,
    input: Input<A>,
    last_movement_tick: u64,
}

pub struct Host<S: Simulation> {
    transport: LyServer,
    simulation: S,
    peers: BTreeMap<ConnKey, Peer<S::Action>>,
    next_player: u64,
    tick: u64,
}

impl<S: Simulation> Host<S> {
    pub fn start(address: SocketAddr, key: [u8; 32], simulation: S) -> Result<Self, String> {
        let transport = LyServer::start_with_protocol(address, key, TICK_HZ, PROTOCOL_ID);
        if !transport.is_bound() {
            return Err(format!("не удалось открыть UDP-сокет {address}"));
        }
        Ok(Self {
            transport,
            simulation,
            peers: BTreeMap::new(),
            next_player: 1,
            tick: 0,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.transport.local_addr()
    }

    /// Вызывается с частотой TICK_HZ. Транспорт и состояние обслуживаются в одном потоке.
    pub fn step(&mut self) -> Result<(), String> {
        self.transport.step();
        self.tick += 1;

        let live: BTreeSet<_> = self.transport.connections().into_iter().collect();
        self.peers.retain(|connection, peer| {
            if live.contains(connection) {
                true
            } else {
                self.simulation.left(peer.player);
                false
            }
        });

        let mut joined = Vec::new();
        for connection in live {
            self.peers.entry(connection).or_insert_with(|| {
                let player = PlayerId(self.next_player);
                self.next_player += 1;
                self.simulation.joined(player);
                joined.push(connection);
                Peer {
                    player,
                    input: Input {
                        movement: [0, 0],
                        action: None,
                    },
                    last_movement_tick: self.tick,
                }
            });
        }

        for (connection, bytes) in self.transport.take_packets() {
            let Some(peer) = self.peers.get_mut(&connection) else {
                continue;
            };
            match decode(&bytes) {
                Some(ClientMessage::Move(movement)) => {
                    peer.input.movement = movement.map(|axis| axis.clamp(-1, 1));
                    peer.last_movement_tick = self.tick;
                }
                Some(ClientMessage::Action(action)) if peer.input.action.is_none() => {
                    peer.input.action = Some(action);
                }
                _ => {}
            }
        }
        let inputs = self
            .peers
            .values_mut()
            .map(|peer| {
                if self.tick - peer.last_movement_tick >= MOVEMENT_TIMEOUT_TICKS {
                    peer.input.movement = [0, 0];
                }
                (
                    peer.player,
                    Input {
                        movement: peer.input.movement,
                        action: peer.input.action.take(),
                    },
                )
            })
            .collect();
        self.simulation.tick(&inputs);

        for connection in joined {
            let welcome = ServerMessage::Welcome {
                player: self.peers[&connection].player,
                tick: self.tick,
                layout: self.simulation.layout(),
                state: self.simulation.state(),
            };
            self.transport.send(connection, true, packet(&welcome)?);
        }
        if !self.peers.is_empty() {
            let state = ServerMessage::<(), _>::State {
                tick: self.tick,
                state: self.simulation.state(),
            };
            let bytes = packet(&state)?;
            for &connection in self.peers.keys() {
                // Следующий снимок заменяет потерянный, так что изменения сходятся и после потерь.
                self.transport.send(connection, false, bytes.clone());
            }
        }
        Ok(())
    }
}

pub struct Session<Layout, State> {
    pub player: PlayerId,
    pub layout: Layout,
    pub state: State,
    tick: u64,
}

pub struct Client<Layout, State, Action> {
    transport: LyClient,
    session: Option<Session<Layout, State>>,
    action: PhantomData<fn(Action)>,
}

impl<Layout: DeserializeOwned, State: DeserializeOwned, Action: Serialize>
    Client<Layout, State, Action>
{
    pub fn connect(address: SocketAddr, client_id: u64, key: [u8; 32]) -> Result<Self, String> {
        Ok(Self {
            transport: LyClient::connect_with_protocol(
                address,
                client_id,
                key,
                TICK_HZ,
                PROTOCOL_ID,
            )?,
            session: None,
            action: PhantomData,
        })
    }

    pub fn step(&mut self) {
        self.transport.step();
        if !self.transport.is_connected() {
            self.session = None;
            return;
        }
        for bytes in self.transport.take_packets() {
            match decode::<ServerMessage<Layout, State>>(&bytes) {
                Some(ServerMessage::Welcome {
                    player,
                    tick,
                    layout,
                    state,
                }) if self.session.is_none() => {
                    self.session = Some(Session {
                        player,
                        layout,
                        state,
                        tick,
                    });
                }
                Some(ServerMessage::State { tick, state }) => {
                    if let Some(session) = self.session.as_mut()
                        && tick > session.tick
                    {
                        session.tick = tick;
                        session.state = state;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn session(&self) -> Option<&Session<Layout, State>> {
        self.session.as_ref()
    }

    pub fn move_player(&mut self, movement: [i8; 2]) {
        self.send(false, ClientMessage::Move(movement));
    }

    pub fn act(&mut self, action: Action) {
        self.send(true, ClientMessage::Action(action));
    }

    fn send(&mut self, reliable: bool, message: ClientMessage<Action>) {
        if self.session.is_some()
            && let Ok(bytes) = packet(&message)
        {
            self.transport.send(reliable, bytes);
        }
    }
}
