//! UDP-транспорт: шифрование ([`crate::secure`]) + надёжный и ненадёжный каналы ([`crate::reliable`]).
//!
//! * часто меняющееся состояние (`EntityState`, `InputAck`, `Input`) идёт по **ненадёжному
//!   секвенированному** каналу — устаревшее не переотправляется;
//! * всё остальное (рукопожатие игры, снимок, операции мира, двери, правки) — по **надёжному
//!   упорядоченному**;
//! * каждая датаграмма зашифрована и аутентифицирована, повторы отбрасываются;
//! * сокет абстрагирован ([`Datagram`]) и время передаётся параметром, поэтому поведение при
//!   потерях и перестановках проверяется детерминированно (`testkit::SimNet`).
//!
//! Датаграмма: `0xA1` + hello1 (клиент → сервер), `0xA2` + hello2 (сервер → клиент),
//! `0xA3` + запись (шифрованный пакет канала).

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};

use astrocore_protocol::ServerMessage;

use crate::reliable::{Channel, ChannelError, MTU_PAYLOAD};
use crate::secure::{
    ClientHandshake, HELLO1_MIN_LEN, MAGIC_HELLO1, MAGIC_HELLO2, SecureError, SecureSession,
    StaticKeypair, client_start, server_accept,
};
use crate::server::{ConnId, GameServer, Out, encode_out};

pub const MAGIC_RECORD: u8 = 0xA3;
/// Максимум датаграмм, обрабатываемых за один шаг (защита от наводнения).
const MAX_DATAGRAMS_PER_STEP: usize = 8192;
/// Максимум одновременных рукопожатий/сессий на сервере.
pub const MAX_SESSIONS: usize = 1024;
const HANDSHAKE_RETRY_MS: u64 = 400;
const HANDSHAKE_TIMEOUT_MS: u64 = 8000;

/// Неблокирующий датаграммный сокет.
pub trait Datagram {
    fn send_to(&mut self, data: &[u8], addr: SocketAddr) -> io::Result<()>;
    /// Следующая принятая датаграмма или `None`, если сейчас ничего нет.
    fn recv_from(&mut self) -> io::Result<Option<(Vec<u8>, SocketAddr)>>;
}

/// Системный UDP-сокет в неблокирующем режиме.
pub struct SystemSocket {
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl SystemSocket {
    pub fn bind(addr: impl std::net::ToSocketAddrs) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            buf: vec![0; 2048],
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

impl Datagram for SystemSocket {
    fn send_to(&mut self, data: &[u8], addr: SocketAddr) -> io::Result<()> {
        match self.socket.send_to(data, addr) {
            Ok(_) => Ok(()),
            // Переполненный буфер отправки — та же потеря, что и в сети: канал переотправит.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn recv_from(&mut self) -> io::Result<Option<(Vec<u8>, SocketAddr)>> {
        match self.socket.recv_from(&mut self.buf) {
            Ok((n, from)) => Ok(Some((self.buf[..n].to_vec(), from))),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            // На некоторых ОС ответ ICMP «порт недоступен» приходит ошибкой чтения: это не сбой сокета.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Надёжно ли доставлять сообщение сервера.
pub fn server_message_is_reliable(msg: &ServerMessage, encoded_len: usize) -> bool {
    !matches!(
        msg,
        ServerMessage::EntityState { .. } | ServerMessage::InputAck { .. }
    ) || encoded_len > MTU_PAYLOAD
}

struct Session {
    conn: ConnId,
    secure: SecureSession,
    channel: Channel,
    hello1: Vec<u8>,
    hello2: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UdpStats {
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub dropped_bad: u64,
    pub handshakes: u64,
    pub sessions_closed: u64,
}

pub struct UdpServer<D: Datagram> {
    socket: D,
    keys: StaticKeypair,
    core: GameServer,
    sessions: HashMap<SocketAddr, Session>,
    by_conn: HashMap<ConnId, SocketAddr>,
    next_conn: u64,
    tick_period_ms: u64,
    next_tick: Option<u64>,
    stats: UdpStats,
}

impl<D: Datagram> UdpServer<D> {
    pub fn new(socket: D, keys: StaticKeypair, core: GameServer, tick_hz: u8) -> Self {
        Self {
            socket,
            keys,
            core,
            sessions: HashMap::new(),
            by_conn: HashMap::new(),
            next_conn: 1,
            tick_period_ms: 1000 / u64::from(tick_hz.max(1)),
            next_tick: None,
            stats: UdpStats::default(),
        }
    }

    pub fn core(&self) -> &GameServer {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut GameServer {
        &mut self.core
    }

    pub fn stats(&self) -> UdpStats {
        self.stats
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn socket(&self) -> &D {
        &self.socket
    }

    /// Один шаг: принять датаграммы, выполнить тики, разослать, обслужить каналы.
    pub fn step(&mut self, now_ms: u64) -> io::Result<()> {
        for _ in 0..MAX_DATAGRAMS_PER_STEP {
            let Some((data, from)) = self.socket.recv_from()? else {
                break;
            };
            self.stats.datagrams_in += 1;
            self.on_datagram(now_ms, &data, from)?;
        }
        let mut next = *self.next_tick.get_or_insert(now_ms);
        let mut ticks = 0;
        while now_ms >= next && ticks < 5 {
            let outs = self.core.tick();
            next += self.tick_period_ms;
            ticks += 1;
            self.dispatch(outs);
        }
        if now_ms >= next + 5 * self.tick_period_ms {
            next = now_ms; // сильно отстали — не догоняем пачкой
        }
        self.next_tick = Some(next);
        self.service_channels(now_ms)
    }

    fn on_datagram(&mut self, now: u64, data: &[u8], from: SocketAddr) -> io::Result<()> {
        match data.first() {
            Some(&MAGIC_HELLO1) => self.on_hello(data, from)?,
            Some(&MAGIC_RECORD) => {
                let Some(session) = self.sessions.get_mut(&from) else {
                    self.stats.dropped_bad += 1;
                    return Ok(());
                };
                match session.secure.open(&data[1..]) {
                    Ok(frame) => match session.channel.on_frame(now, &frame) {
                        Ok(()) => {
                            let conn = session.conn;
                            let msgs = session.channel.take_messages();
                            for m in msgs {
                                let outs = self.core.on_packet(conn, &m);
                                self.dispatch(outs);
                            }
                        }
                        Err(_) => self.close_addr(from),
                    },
                    Err(_) => self.stats.dropped_bad += 1,
                }
            }
            _ => self.stats.dropped_bad += 1,
        }
        Ok(())
    }

    fn on_hello(&mut self, hello1: &[u8], from: SocketAddr) -> io::Result<()> {
        if let Some(existing) = self.sessions.get(&from) {
            if existing.hello1 == hello1 {
                // Клиент повторил первый пакет (ответ потерялся) — повторяем тот же ответ.
                let reply = existing.hello2.clone();
                self.stats.datagrams_out += 1;
                return self.socket.send_to(&reply, from);
            }
            self.close_addr(from); // тот же адрес начал новое рукопожатие
        }
        if self.sessions.len() >= MAX_SESSIONS || hello1.len() < HELLO1_MIN_LEN {
            self.stats.dropped_bad += 1;
            return Ok(());
        }
        let (secure, hello2) = match server_accept(&self.keys, hello1) {
            Ok(v) => v,
            Err(_) => {
                self.stats.dropped_bad += 1;
                return Ok(());
            }
        };
        let conn = ConnId(self.next_conn);
        self.next_conn += 1;
        self.stats.handshakes += 1;
        self.stats.datagrams_out += 1;
        self.socket.send_to(&hello2, from)?;
        self.sessions.insert(
            from,
            Session {
                conn,
                secure,
                channel: Channel::new(),
                hello1: hello1.to_vec(),
                hello2,
            },
        );
        self.by_conn.insert(conn, from);
        let outs = self.core.connect(conn);
        self.dispatch(outs);
        Ok(())
    }

    fn close_addr(&mut self, addr: SocketAddr) {
        if let Some(s) = self.sessions.remove(&addr) {
            self.by_conn.remove(&s.conn);
            self.stats.sessions_closed += 1;
            let outs = self.core.disconnect(s.conn);
            self.dispatch(outs);
        }
    }

    /// Разложить результат ядра по каналам сессий.
    fn dispatch(&mut self, outs: Vec<Out>) {
        let mut pending = outs;
        while !pending.is_empty() {
            let mut more = Vec::new();
            for out in pending {
                match out {
                    Out::Send { to, msg } => {
                        let (Some(addr), Some(bytes)) =
                            (self.by_conn.get(&to).copied(), encode_out(&msg))
                        else {
                            continue;
                        };
                        if let Some(s) = self.sessions.get_mut(&addr) {
                            let sent = if server_message_is_reliable(&msg, bytes.len()) {
                                s.channel.send_reliable(&bytes)
                            } else {
                                s.channel.send_unreliable(&bytes)
                            };
                            if sent.is_err() {
                                // Переполненная очередь у медленного клиента: отключаем его.
                                more.push(Out::Disconnect(to));
                            }
                        }
                    }
                    Out::Disconnect(conn) => {
                        if let Some(addr) = self.by_conn.remove(&conn)
                            && self.sessions.remove(&addr).is_some()
                        {
                            self.stats.sessions_closed += 1;
                            more.extend(self.core.disconnect(conn));
                        }
                    }
                }
            }
            pending = more;
        }
    }

    fn service_channels(&mut self, now: u64) -> io::Result<()> {
        let addrs: Vec<SocketAddr> = self.sessions.keys().copied().collect();
        for addr in addrs {
            let Some(s) = self.sessions.get_mut(&addr) else {
                continue;
            };
            match s.channel.poll(now) {
                Ok(frames) => {
                    for f in frames {
                        let Ok(rec) = s.secure.seal(&f) else { continue };
                        let mut dg = Vec::with_capacity(1 + rec.len());
                        dg.push(MAGIC_RECORD);
                        dg.extend_from_slice(&rec);
                        self.stats.datagrams_out += 1;
                        self.socket.send_to(&dg, addr)?;
                    }
                }
                Err(_) => self.close_addr(addr),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("рукопожатие: {0}")]
    Secure(#[from] SecureError),
    #[error("сервер не ответил на рукопожатие")]
    HandshakeTimeout,
    #[error("канал: {0}")]
    Channel(#[from] ChannelError),
}

enum ClientState {
    Connecting {
        hs: ClientHandshake,
        hello1: Vec<u8>,
        started: Option<u64>,
        last_try: Option<u64>,
    },
    Established {
        secure: Box<SecureSession>,
        channel: Box<Channel>,
    },
    Failed(ClientError),
}

pub struct UdpClient<D: Datagram> {
    socket: D,
    server: SocketAddr,
    state: ClientState,
    queued: Vec<(bool, Vec<u8>)>,
    received: Vec<Vec<u8>>,
}

impl<D: Datagram> UdpClient<D> {
    /// `server_public` — закреплённый открытый ключ сервера.
    pub fn connect(
        socket: D,
        server: SocketAddr,
        server_public: [u8; 32],
    ) -> Result<Self, ClientError> {
        let (hs, hello1) = client_start(server_public)?;
        Ok(Self {
            socket,
            server,
            state: ClientState::Connecting {
                hs,
                hello1,
                started: None,
                last_try: None,
            },
            queued: Vec::new(),
            received: Vec::new(),
        })
    }

    pub fn is_established(&self) -> bool {
        matches!(self.state, ClientState::Established { .. })
    }

    pub fn error(&self) -> Option<&ClientError> {
        match &self.state {
            ClientState::Failed(e) => Some(e),
            _ => None,
        }
    }

    pub fn socket(&self) -> &D {
        &self.socket
    }

    /// Отправить сообщение: `reliable` — по надёжному каналу. До установки соединения
    /// сообщения ждут в очереди.
    pub fn send(&mut self, reliable: bool, data: &[u8]) -> Result<(), ChannelError> {
        match &mut self.state {
            ClientState::Established { channel, .. } => {
                if reliable || data.len() > MTU_PAYLOAD {
                    channel.send_reliable(data)
                } else {
                    channel.send_unreliable(data)
                }
            }
            _ => {
                if self.queued.len() >= 256 {
                    return Err(ChannelError::Backlog);
                }
                self.queued.push((reliable, data.to_vec()));
                Ok(())
            }
        }
    }

    /// Принятые сообщения сервера.
    pub fn take_packets(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.received)
    }

    pub fn step(&mut self, now_ms: u64) -> io::Result<()> {
        for _ in 0..MAX_DATAGRAMS_PER_STEP {
            let Some((data, from)) = self.socket.recv_from()? else {
                break;
            };
            if from != self.server {
                continue; // чужой источник
            }
            self.on_datagram(now_ms, &data);
        }
        match &mut self.state {
            ClientState::Connecting {
                hello1,
                started,
                last_try,
                ..
            } => {
                let began = *started.get_or_insert(now_ms);
                if now_ms.saturating_sub(began) > HANDSHAKE_TIMEOUT_MS {
                    self.state = ClientState::Failed(ClientError::HandshakeTimeout);
                } else if last_try.is_none_or(|t| now_ms.saturating_sub(t) >= HANDSHAKE_RETRY_MS) {
                    self.socket.send_to(hello1, self.server)?;
                    *last_try = Some(now_ms);
                }
            }
            ClientState::Established { secure, channel } => match channel.poll(now_ms) {
                Ok(frames) => {
                    for f in frames {
                        let Ok(rec) = secure.seal(&f) else { continue };
                        let mut dg = Vec::with_capacity(1 + rec.len());
                        dg.push(MAGIC_RECORD);
                        dg.extend_from_slice(&rec);
                        self.socket.send_to(&dg, self.server)?;
                    }
                }
                Err(e) => self.state = ClientState::Failed(e.into()),
            },
            ClientState::Failed(_) => {}
        }
        Ok(())
    }

    fn on_datagram(&mut self, now: u64, data: &[u8]) {
        match (&mut self.state, data.first()) {
            (ClientState::Connecting { .. }, Some(&MAGIC_HELLO2)) => {
                let ClientState::Connecting { hs, .. } = std::mem::replace(
                    &mut self.state,
                    ClientState::Failed(ClientError::HandshakeTimeout),
                ) else {
                    return;
                };
                match hs.finish(data) {
                    Ok(secure) => {
                        let mut channel = Channel::new();
                        for (reliable, d) in self.queued.drain(..) {
                            let _ = if reliable || d.len() > MTU_PAYLOAD {
                                channel.send_reliable(&d)
                            } else {
                                channel.send_unreliable(&d)
                            };
                        }
                        let _ = now;
                        self.state = ClientState::Established {
                            secure: Box::new(secure),
                            channel: Box::new(channel),
                        };
                    }
                    Err(e) => self.state = ClientState::Failed(e.into()),
                }
            }
            (ClientState::Established { secure, channel }, Some(&MAGIC_RECORD)) => {
                if let Ok(frame) = secure.open(&data[1..])
                    && let Err(e) = channel.on_frame(now, &frame)
                {
                    self.state = ClientState::Failed(e.into());
                    return;
                }
                self.received.extend(channel.take_messages());
            }
            _ => {}
        }
    }
}

#[allow(dead_code)]
const _: () = assert!(MAGIC_HELLO1 != MAGIC_HELLO2 && MAGIC_HELLO2 != MAGIC_RECORD);
