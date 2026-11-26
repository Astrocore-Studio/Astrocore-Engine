//! Серверная симуляция с текущим снимком состояния и неизменяемой картой на время сессии.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

use astrocore_lightyear::{ConnKey, LyClient, LyServer};
use astrocore_protocol::MAX_PACKET_BYTES;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const PROTOCOL_NAMESPACE: u64 = 0x4153_5452_4402_0000;
const MOVEMENT_TIMEOUT_TICKS: u64 = 5;
const MAX_PENDING_ACTIONS: usize = 16;
const MAX_LAYOUT_BYTES: usize = 512 * 1024;
const LAYOUT_PART_BYTES: usize = 8 * 1024;
// Полный снимок должен уйти до стандартного отключения Lightyear через 3 секунды.
const HISTORY_TICKS: u64 = 32;
pub const TICK_HZ: u32 = 20;
pub const TICK_PERIOD: Duration = Duration::from_millis(1000 / TICK_HZ as u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(u64);

#[derive(Debug)]
pub struct Input<A> {
    pub movement: [i8; 2],
    pub action: Option<A>,
}

/// Снимок, который можно восстановить из точной копии базового тика и дельты.
pub trait ReplicatedState: Clone + Serialize + DeserializeOwned {
    type Delta: Serialize + DeserializeOwned;

    fn diff(&self, baseline: &Self) -> Self::Delta;
    fn apply(&mut self, delta: Self::Delta);
}

pub trait Simulation {
    const WIRE_VERSION: u16;

    type Layout: Serialize;
    type State: ReplicatedState;
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
    Ack(u64),
}

#[derive(Serialize, Deserialize)]
enum ServerMessage<'a, State, Delta> {
    Welcome { player: PlayerId, layout_bytes: u32 },
    LayoutPart { bytes: &'a [u8] },
    State { tick: u64, state: State },
    Delta { base: u64, tick: u64, delta: Delta },
}

fn packet(message: &impl Serialize) -> Result<Vec<u8>, String> {
    let bytes = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PACKET_BYTES {
        return Err(format!("пакет 2D превышает лимит {MAX_PACKET_BYTES} байт"));
    }
    Ok(bytes)
}

fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    if bytes.len() > MAX_PACKET_BYTES {
        return None;
    }
    deserialize_exact(bytes)
}

fn deserialize_exact<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    let (message, rest) = postcard::take_from_bytes(bytes).ok()?;
    rest.is_empty().then_some(message)
}

fn prune_history<State>(history: &mut BTreeMap<u64, State>, tick: u64) {
    let oldest = tick.saturating_sub(HISTORY_TICKS - 1);
    while history
        .first_key_value()
        .is_some_and(|(&key, _)| key < oldest)
    {
        history.pop_first();
    }
}

struct Peer<A> {
    player: PlayerId,
    movement: [i8; 2],
    actions: VecDeque<A>,
    last_movement_tick: u64,
    bootstrap: Bootstrap,
    acknowledged: Option<u64>,
    last_sent: u64,
    next_full: u64,
}

enum Bootstrap {
    Welcome,
    Layout(usize),
    State,
    Complete,
}

pub struct Host<S: Simulation> {
    transport: LyServer,
    simulation: S,
    layout_bytes: u32,
    layout_packets: Vec<Vec<u8>>,
    peers: BTreeMap<ConnKey, Peer<S::Action>>,
    history: BTreeMap<u64, S::State>,
    next_player: u64,
    tick: u64,
}

impl<S: Simulation> Host<S> {
    pub fn start(address: SocketAddr, key: [u8; 32], simulation: S) -> Result<Self, String> {
        let layout = postcard::to_stdvec(simulation.layout()).map_err(|error| error.to_string())?;
        if layout.len() > MAX_LAYOUT_BYTES {
            return Err(format!("карта 2D превышает лимит {MAX_LAYOUT_BYTES} байт"));
        }
        let layout_bytes = layout.len() as u32;
        let layout_packets = layout
            .chunks(LAYOUT_PART_BYTES)
            .map(|bytes| packet(&ServerMessage::<(), ()>::LayoutPart { bytes }))
            .collect::<Result<_, _>>()?;
        let transport = LyServer::start_with_protocol(
            address,
            key,
            TICK_HZ,
            PROTOCOL_NAMESPACE | u64::from(S::WIRE_VERSION),
        );
        if !transport.is_bound() {
            return Err(format!("не удалось открыть UDP-сокет {address}"));
        }
        Ok(Self {
            transport,
            simulation,
            layout_bytes,
            layout_packets,
            peers: BTreeMap::new(),
            history: BTreeMap::new(),
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

        for connection in live {
            self.peers.entry(connection).or_insert_with(|| {
                let player = PlayerId(self.next_player);
                self.next_player += 1;
                self.simulation.joined(player);
                Peer {
                    player,
                    movement: [0, 0],
                    actions: VecDeque::new(),
                    last_movement_tick: self.tick,
                    bootstrap: Bootstrap::Welcome,
                    acknowledged: None,
                    last_sent: 0,
                    next_full: 0,
                }
            });
        }

        for (connection, bytes) in self.transport.take_packets() {
            let Some(peer) = self.peers.get_mut(&connection) else {
                continue;
            };
            if !matches!(peer.bootstrap, Bootstrap::Complete) {
                continue;
            }
            match decode(&bytes) {
                Some(ClientMessage::Move(movement)) => {
                    peer.movement = movement.map(|axis| axis.clamp(-1, 1));
                    peer.last_movement_tick = self.tick;
                }
                Some(ClientMessage::Action(action)) if peer.actions.len() < MAX_PENDING_ACTIONS => {
                    peer.actions.push_back(action);
                }
                Some(ClientMessage::Ack(tick))
                    if tick <= peer.last_sent
                        && peer.acknowledged.is_none_or(|base| tick > base)
                        && self.history.contains_key(&tick) =>
                {
                    peer.acknowledged = Some(tick);
                }
                _ => {}
            }
        }
        let inputs = self
            .peers
            .values_mut()
            .map(|peer| {
                if self.tick - peer.last_movement_tick >= MOVEMENT_TIMEOUT_TICKS {
                    peer.movement = [0, 0];
                }
                (
                    peer.player,
                    Input {
                        movement: peer.movement,
                        action: peer.actions.pop_front(),
                    },
                )
            })
            .collect();
        self.simulation.tick(&inputs);
        self.publish()
    }

    fn publish(&mut self) -> Result<(), String> {
        let tick = self.tick;
        let snapshot = self.simulation.state();
        let mut full = None;
        let mut deltas = BTreeMap::new();
        let mut published = false;
        prune_history(&mut self.history, tick);
        for (&connection, peer) in &mut self.peers {
            let (bytes, next) = match peer.bootstrap {
                Bootstrap::Welcome => {
                    let welcome = ServerMessage::<(), ()>::Welcome {
                        player: peer.player,
                        layout_bytes: self.layout_bytes,
                    };
                    let next = if self.layout_packets.is_empty() {
                        Bootstrap::State
                    } else {
                        Bootstrap::Layout(0)
                    };
                    (packet(&welcome)?, next)
                }
                Bootstrap::Layout(part) => {
                    let next = if part + 1 == self.layout_packets.len() {
                        Bootstrap::State
                    } else {
                        Bootstrap::Layout(part + 1)
                    };
                    (self.layout_packets[part].clone(), next)
                }
                Bootstrap::State | Bootstrap::Complete => {
                    let baseline = peer
                        .acknowledged
                        .and_then(|base| self.history.get(&base).map(|state| (base, state)));
                    let delta = if let Some((base, baseline)) = baseline {
                        deltas
                            .entry(base)
                            .or_insert_with(|| {
                                packet(&ServerMessage::<(), _>::Delta {
                                    base,
                                    tick,
                                    delta: snapshot.diff(baseline),
                                })
                                .ok()
                            })
                            .as_ref()
                    } else {
                        None
                    };
                    let (reliable, bytes) = if let Some(bytes) = delta {
                        (false, bytes)
                    } else {
                        if tick < peer.next_full {
                            continue;
                        }
                        if full.is_none() {
                            full = Some(packet(&ServerMessage::<_, ()>::State {
                                tick,
                                state: snapshot,
                            })?);
                        }
                        (true, full.as_ref().expect("полный снимок подготовлен"))
                    };
                    if self.transport.send(connection, reliable, bytes.clone()) {
                        peer.bootstrap = Bootstrap::Complete;
                        peer.last_sent = tick;
                        if reliable {
                            peer.next_full = tick + HISTORY_TICKS;
                        }
                        published = true;
                    }
                    continue;
                }
            };
            if self.transport.send(connection, true, bytes) {
                peer.bootstrap = next;
            }
        }
        if published {
            self.history.insert(tick, snapshot.clone());
        }
        Ok(())
    }
}

pub struct Session<Layout, State> {
    pub player: PlayerId,
    pub layout: Layout,
    pub state: State,
    tick: u64,
    history: BTreeMap<u64, State>,
}

impl<Layout, State: ReplicatedState> Session<Layout, State> {
    fn update(&mut self, tick: u64, state: State) {
        self.history
            .insert(self.tick, std::mem::replace(&mut self.state, state));
        self.tick = tick;
        prune_history(&mut self.history, tick);
    }

    fn apply_delta(&mut self, base: u64, tick: u64, delta: State::Delta) {
        if tick <= self.tick || base >= tick {
            return;
        }
        let baseline = if base == self.tick {
            &self.state
        } else if let Some(state) = self.history.get(&base) {
            state
        } else {
            return;
        };
        let mut state = baseline.clone();
        state.apply(delta);
        self.update(tick, state);
    }
}

pub struct Client<Layout, State, Action> {
    transport: LyClient,
    session: Option<Session<Layout, State>>,
    pending: Option<PendingLayout<Layout>>,
    action: PhantomData<fn(Action)>,
}

struct PendingLayout<Layout> {
    player: PlayerId,
    len: usize,
    bytes: Vec<u8>,
    layout: Option<Layout>,
}

impl<Layout: DeserializeOwned> PendingLayout<Layout> {
    fn receive(&mut self, bytes: &[u8]) {
        let left = self.len - self.bytes.len();
        if self.layout.is_some() || bytes.len() != left.min(LAYOUT_PART_BYTES) {
            return;
        }
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() == self.len {
            self.layout = deserialize_exact(&self.bytes);
        }
    }
}

impl<Layout: DeserializeOwned, State: ReplicatedState, Action: Serialize>
    Client<Layout, State, Action>
{
    pub fn connect(
        address: SocketAddr,
        client_id: u64,
        key: [u8; 32],
        wire_version: u16,
    ) -> Result<Self, String> {
        Ok(Self {
            transport: LyClient::connect_with_protocol(
                address,
                client_id,
                key,
                TICK_HZ,
                PROTOCOL_NAMESPACE | u64::from(wire_version),
            )?,
            session: None,
            pending: None,
            action: PhantomData,
        })
    }

    pub fn step(&mut self) {
        self.transport.step();
        if !self.transport.is_connected() {
            self.session = None;
            self.pending = None;
            return;
        }
        let previous_tick = self.session.as_ref().map(|session| session.tick);
        for bytes in self.transport.take_packets() {
            match decode::<ServerMessage<State, State::Delta>>(&bytes) {
                Some(ServerMessage::Welcome {
                    player,
                    layout_bytes,
                }) if self.session.is_none()
                    && self.pending.is_none()
                    && layout_bytes as usize <= MAX_LAYOUT_BYTES =>
                {
                    let mut pending = PendingLayout {
                        player,
                        len: layout_bytes as usize,
                        bytes: Vec::new(),
                        layout: None,
                    };
                    if pending.len == 0 {
                        pending.receive(&[]);
                    }
                    self.pending = Some(pending);
                }
                Some(ServerMessage::LayoutPart { bytes }) => {
                    if let Some(pending) = self.pending.as_mut() {
                        pending.receive(bytes);
                    }
                }
                Some(ServerMessage::State { tick, state }) => {
                    if let Some(session) = self.session.as_mut() {
                        if tick > session.tick {
                            session.update(tick, state);
                        }
                    } else if let Some(PendingLayout {
                        player,
                        layout: Some(layout),
                        ..
                    }) = self.pending.take_if(|pending| pending.layout.is_some())
                    {
                        self.session = Some(Session {
                            player,
                            layout,
                            state,
                            tick,
                            history: BTreeMap::new(),
                        });
                    }
                }
                Some(ServerMessage::Delta { base, tick, delta }) => {
                    if let Some(session) = self.session.as_mut() {
                        session.apply_delta(base, tick, delta);
                    }
                }
                _ => {}
            }
        }
        if let Some(tick) = self.session.as_ref().map(|session| session.tick)
            && previous_tick != Some(tick)
        {
            self.send(true, ClientMessage::Ack(tick));
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
