use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use astrocore_protocol::MAX_PACKET_BYTES;

use crate::server::{ConnId, GameServer, Out, encode_out};

const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

pub fn write_frame(stream: &mut impl Write, packet: &[u8]) -> io::Result<()> {
    if packet.len() > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "кадр больше лимита",
        ));
    }
    stream.write_all(&(packet.len() as u32).to_le_bytes())?;
    stream.write_all(packet)
}

/// Прочитать один кадр. Кадр длиннее лимита — ошибка без выделения памяти.
pub fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "кадр больше лимита",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

enum Event {
    Packet(ConnId, Vec<u8>),
    Closed(ConnId),
}

pub struct TcpServer {
    listener: TcpListener,
    core: GameServer,
    conns: BTreeMap<ConnId, TcpStream>,
    next_conn: u64,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
}

impl TcpServer {
    pub fn bind(addr: impl ToSocketAddrs, core: GameServer) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let (events_tx, events_rx) = channel();
        Ok(Self {
            listener,
            core,
            conns: BTreeMap::new(),
            next_conn: 1,
            events_tx,
            events_rx,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn core(&self) -> &GameServer {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut GameServer {
        &mut self.core
    }

    pub fn connection_count(&self) -> usize {
        self.conns.len()
    }

    /// Один шаг: принять соединения, разобрать входящие пакеты, выполнить тик, разослать результат.
    pub fn step(&mut self) -> io::Result<()> {
        self.accept_all()?;
        let mut outs = Vec::new();
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                Event::Packet(conn, bytes) if self.conns.contains_key(&conn) => {
                    outs.extend(self.core.on_packet(conn, &bytes));
                }
                Event::Closed(conn) => {
                    if self.conns.remove(&conn).is_some() {
                        outs.extend(self.core.disconnect(conn));
                    }
                }
                Event::Packet(..) => {}
            }
        }
        outs.extend(self.core.tick());
        self.flush(outs);
        Ok(())
    }

    /// Работать с фиксированной частотой тика. `ticks = None` — бесконечно.
    pub fn run(&mut self, ticks: Option<u64>) -> io::Result<()> {
        let period = Duration::from_secs_f64(1.0 / f64::from(self.core_tick_hz()));
        let mut next = Instant::now();
        let mut done = 0u64;
        while ticks.is_none_or(|t| done < t) {
            self.step()?;
            done += 1;
            next += period;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            } else {
                next = Instant::now(); // отстали — не пытаемся «догонять» пачкой тиков
            }
        }
        Ok(())
    }

    fn core_tick_hz(&self) -> u8 {
        20
    }

    fn accept_all(&mut self) -> io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let id = ConnId(self.next_conn);
                    self.next_conn += 1;
                    stream.set_nodelay(true).ok();
                    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let tx = self.events_tx.clone();
                    thread::spawn(move || reader_loop(id, reader, tx));
                    self.conns.insert(id, stream);
                    let outs = self.core.connect(id);
                    self.flush(outs);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn flush(&mut self, outs: Vec<Out>) {
        let mut more = Vec::new();
        for out in outs {
            match out {
                Out::Send { to, msg } => {
                    let Some(bytes) = encode_out(&msg) else {
                        continue;
                    };
                    let failed = match self.conns.get_mut(&to) {
                        Some(stream) => write_frame(stream, &bytes).is_err(),
                        None => false,
                    };
                    if failed {
                        self.drop_conn(to, &mut more);
                    }
                }
                Out::Disconnect(conn) => self.drop_conn(conn, &mut more),
            }
        }
        if !more.is_empty() {
            self.flush(more);
        }
    }

    fn drop_conn(&mut self, conn: ConnId, more: &mut Vec<Out>) {
        if let Some(stream) = self.conns.remove(&conn) {
            stream.shutdown(std::net::Shutdown::Both).ok();
            more.extend(self.core.disconnect(conn));
        }
    }
}

fn reader_loop(id: ConnId, mut stream: TcpStream, tx: Sender<Event>) {
    while let Ok(frame) = read_frame(&mut stream) {
        if tx.send(Event::Packet(id, frame)).is_err() {
            return;
        }
    }
    let _ = tx.send(Event::Closed(id));
}

pub struct TcpClient {
    stream: TcpStream,
    rx: Receiver<Vec<u8>>,
    closed: Arc<AtomicBool>,
}

impl TcpClient {
    pub fn connect(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
        let mut reader = stream.try_clone()?;
        let (tx, rx) = channel();
        let closed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&closed);
        thread::spawn(move || {
            while let Ok(frame) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
            flag.store(true, Ordering::SeqCst);
        });
        Ok(Self { stream, rx, closed })
    }

    pub fn send(&mut self, packet: &[u8]) -> io::Result<()> {
        write_frame(&mut self.stream, packet)
    }

    /// Все полученные на данный момент пакеты.
    pub fn poll(&self) -> Vec<Vec<u8>> {
        self.rx.try_iter().collect()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip_and_respect_limits() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut cur = io::Cursor::new(buf);
        assert_eq!(read_frame(&mut cur).unwrap(), b"hello");
        assert_eq!(read_frame(&mut cur).unwrap(), b"");
        assert!(read_frame(&mut cur).is_err(), "конец потока");

        assert!(write_frame(&mut Vec::new(), &vec![0; MAX_PACKET_BYTES + 1]).is_err());
        // Заявленная огромная длина отвергается без выделения памяти.
        let mut huge = io::Cursor::new(u32::MAX.to_le_bytes().to_vec());
        assert_eq!(
            read_frame(&mut huge).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Обрыв посреди кадра.
        let mut cut = io::Cursor::new(vec![10, 0, 0, 0, 1, 2]);
        assert_eq!(
            read_frame(&mut cut).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
