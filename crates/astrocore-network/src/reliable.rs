//! Надёжный упорядоченный и ненадёжный секвенированный каналы поверх датаграмм.
//!
//! Чистый конечный автомат: без сокетов и часов — время передаётся параметром (мс),
//! поэтому поведение при потерях, дублях и перестановках тестируется детерминированно.
//!
//! * **Надёжный упорядоченный** канал: сообщение режется на фрагменты по [`MTU_PAYLOAD`]
//!   байт; каждый фрагмент — отдельный пакет с последовательным номером; получатель подтверждает
//!   (накопленный ack + битовая карта следующих 64) и собирает сообщения строго по порядку;
//!   неподтверждённое переотправляется с растущим таймаутом;
//! * **ненадёжный секвенированный** канал: одно сообщение — один пакет; устаревшие (старше уже
//!   принятого) отбрасываются — для часто меняющегося состояния;
//! * keepalive и обнаружение «мёртвого» собеседника.

use std::collections::{BTreeMap, VecDeque};

pub const MTU_PAYLOAD: usize = 1000;
pub const MAX_MESSAGE_BYTES: usize = 128 * 1024;
pub const MAX_FRAGMENTS: usize = MAX_MESSAGE_BYTES.div_ceil(MTU_PAYLOAD);
const SEND_WINDOW: usize = 128;
const RECV_WINDOW: u32 = 256;
const MAX_BACKLOG_BYTES: usize = 4 * 1024 * 1024;
const INITIAL_RTO_MS: u64 = 150;
const MAX_RTO_MS: u64 = 1000;
const MAX_TRIES: u32 = 40;
const KEEPALIVE_MS: u64 = 2000;
pub const PEER_TIMEOUT_MS: u64 = 10_000;

const T_RELIABLE: u8 = 1;
const T_UNRELIABLE: u8 = 2;
const T_ACK: u8 = 3;
const T_PING: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("сообщение слишком велико ({0} байт)")]
    TooLarge(usize),
    #[error("очередь отправки переполнена")]
    Backlog,
    #[error("собеседник не отвечает")]
    Timeout,
    #[error("нарушение протокола канала: {0}")]
    Protocol(&'static str),
}

struct Inflight {
    frame: Vec<u8>,
    sent_at: u64,
    rto: u64,
    tries: u32,
}

#[derive(Default)]
pub struct Channel {
    // отправка
    next_seq: u32,
    queue: VecDeque<(u16, u16, Vec<u8>)>,
    queued_bytes: usize,
    inflight: BTreeMap<u32, Inflight>,
    outbox: Vec<Vec<u8>>,
    unreliable_seq: u32,
    last_sent: u64,
    // приём
    recv_next: u32,
    pending: BTreeMap<u32, (u16, u16, Vec<u8>)>,
    assembling: Vec<u8>,
    expected_idx: u16,
    unreliable_latest: Option<u32>,
    delivered: Vec<Vec<u8>>,
    ack_dirty: bool,
    last_recv: Option<u64>,
    started: Option<u64>,
}

fn seq_after(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

impl Channel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    pub fn backlog_len(&self) -> usize {
        self.queue.len()
    }

    /// Всё отправленное подтверждено и очередь пуста.
    pub fn is_idle(&self) -> bool {
        self.inflight.is_empty() && self.queue.is_empty()
    }

    /// Поставить сообщение в очередь надёжной доставки.
    pub fn send_reliable(&mut self, msg: &[u8]) -> Result<(), ChannelError> {
        if msg.len() > MAX_MESSAGE_BYTES {
            return Err(ChannelError::TooLarge(msg.len()));
        }
        if self.queued_bytes + msg.len() > MAX_BACKLOG_BYTES {
            return Err(ChannelError::Backlog);
        }
        let chunks: Vec<&[u8]> = if msg.is_empty() {
            vec![&[][..]]
        } else {
            msg.chunks(MTU_PAYLOAD).collect()
        };
        let count = chunks.len() as u16;
        for (i, c) in chunks.into_iter().enumerate() {
            self.queue.push_back((i as u16, count, c.to_vec()));
        }
        self.queued_bytes += msg.len();
        Ok(())
    }

    /// Ненадёжное секвенированное сообщение (один пакет).
    pub fn send_unreliable(&mut self, msg: &[u8]) -> Result<(), ChannelError> {
        if msg.len() > MTU_PAYLOAD {
            return Err(ChannelError::TooLarge(msg.len()));
        }
        self.unreliable_seq = self.unreliable_seq.wrapping_add(1);
        let mut f = Vec::with_capacity(5 + msg.len());
        f.push(T_UNRELIABLE);
        f.extend_from_slice(&self.unreliable_seq.to_be_bytes());
        f.extend_from_slice(msg);
        self.outbox.push(f);
        Ok(())
    }

    /// Принять расшифрованный пакет канала.
    pub fn on_frame(&mut self, now_ms: u64, frame: &[u8]) -> Result<(), ChannelError> {
        self.last_recv = Some(now_ms);
        let Some((&kind, body)) = frame.split_first() else {
            return Err(ChannelError::Protocol("пустой пакет"));
        };
        match kind {
            T_RELIABLE => self.on_reliable(body),
            T_UNRELIABLE => {
                let (seq, data) = body
                    .split_first_chunk::<4>()
                    .ok_or(ChannelError::Protocol("короткий пакет"))?;
                let seq = u32::from_be_bytes(*seq);
                if self.unreliable_latest.is_none_or(|l| seq_after(seq, l)) {
                    self.unreliable_latest = Some(seq);
                    self.delivered.push(data.to_vec());
                }
                Ok(())
            }
            T_ACK => self.on_ack(body),
            T_PING => {
                self.ack_dirty = true;
                Ok(())
            }
            _ => Err(ChannelError::Protocol("неизвестный тип пакета")),
        }
    }

    fn on_reliable(&mut self, body: &[u8]) -> Result<(), ChannelError> {
        if body.len() < 8 {
            return Err(ChannelError::Protocol("короткий пакет данных"));
        }
        let seq = u32::from_be_bytes(body[..4].try_into().expect("длина проверена"));
        let idx = u16::from_be_bytes(body[4..6].try_into().expect("длина проверена"));
        let count = u16::from_be_bytes(body[6..8].try_into().expect("длина проверена"));
        if count == 0
            || usize::from(count) > MAX_FRAGMENTS
            || idx >= count
            || body.len() - 8 > MTU_PAYLOAD
        {
            return Err(ChannelError::Protocol("неверный фрагмент"));
        }
        self.ack_dirty = true;
        let ahead = seq.wrapping_sub(self.recv_next);
        if ahead >= 0x8000_0000 {
            return Ok(()); // уже получен (дубль): просто переподтвердим
        }
        if ahead >= RECV_WINDOW {
            return Ok(()); // слишком далеко вперёд — отправитель переотправит
        }
        self.pending
            .entry(seq)
            .or_insert((idx, count, body[8..].to_vec()));
        while let Some((idx, count, data)) = self.pending.remove(&self.recv_next) {
            self.recv_next = self.recv_next.wrapping_add(1);
            if idx != self.expected_idx {
                return Err(ChannelError::Protocol("нарушен порядок фрагментов"));
            }
            self.assembling.extend_from_slice(&data);
            if self.assembling.len() > MAX_MESSAGE_BYTES {
                return Err(ChannelError::Protocol("сообщение превышает лимит"));
            }
            if idx + 1 == count {
                self.delivered.push(std::mem::take(&mut self.assembling));
                self.expected_idx = 0;
            } else {
                self.expected_idx = idx + 1;
            }
        }
        Ok(())
    }

    fn on_ack(&mut self, body: &[u8]) -> Result<(), ChannelError> {
        if body.len() != 12 {
            return Err(ChannelError::Protocol("неверный ack"));
        }
        let cum = u32::from_be_bytes(body[..4].try_into().expect("длина проверена"));
        let bits = u64::from_be_bytes(body[4..12].try_into().expect("длина проверена"));
        self.inflight.retain(|&seq, _| {
            let before_cum = cum.wrapping_sub(seq).wrapping_sub(1) < 0x8000_0000; // seq < cum
            if before_cum {
                return false;
            }
            let off = seq.wrapping_sub(cum);
            !(1..=64).contains(&off) || bits & (1u64 << (off - 1)) == 0
        });
        Ok(())
    }

    /// Пакеты, которые нужно отправить сейчас: новые данные в пределах окна, переотправки,
    /// подтверждения, keepalive. Ошибка — собеседник не отвечает.
    pub fn poll(&mut self, now_ms: u64) -> Result<Vec<Vec<u8>>, ChannelError> {
        let started = *self.started.get_or_insert(now_ms);
        if now_ms.saturating_sub(self.last_recv.unwrap_or(started)) > PEER_TIMEOUT_MS {
            return Err(ChannelError::Timeout);
        }
        let mut out = std::mem::take(&mut self.outbox);

        // Переотправки.
        for inf in self.inflight.values_mut() {
            if now_ms.saturating_sub(inf.sent_at) >= inf.rto {
                inf.tries += 1;
                if inf.tries > MAX_TRIES {
                    return Err(ChannelError::Timeout);
                }
                inf.sent_at = now_ms;
                inf.rto = (inf.rto * 2).min(MAX_RTO_MS);
                out.push(inf.frame.clone());
            }
        }
        // Новые данные в пределах окна.
        while self.inflight.len() < SEND_WINDOW {
            let Some((idx, count, data)) = self.queue.pop_front() else {
                break;
            };
            self.queued_bytes = self.queued_bytes.saturating_sub(data.len());
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            let mut f = Vec::with_capacity(9 + data.len());
            f.push(T_RELIABLE);
            f.extend_from_slice(&seq.to_be_bytes());
            f.extend_from_slice(&idx.to_be_bytes());
            f.extend_from_slice(&count.to_be_bytes());
            f.extend_from_slice(&data);
            out.push(f.clone());
            self.inflight.insert(
                seq,
                Inflight {
                    frame: f,
                    sent_at: now_ms,
                    rto: INITIAL_RTO_MS,
                    tries: 0,
                },
            );
        }
        // Подтверждение.
        if self.ack_dirty {
            self.ack_dirty = false;
            let mut bits = 0u64;
            for (&seq, _) in self.pending.iter() {
                let off = seq.wrapping_sub(self.recv_next);
                if (1..=64).contains(&off) {
                    bits |= 1u64 << (off - 1);
                }
            }
            let mut f = vec![T_ACK];
            f.extend_from_slice(&self.recv_next.to_be_bytes());
            f.extend_from_slice(&bits.to_be_bytes());
            out.push(f);
        }
        if out.is_empty() && now_ms.saturating_sub(self.last_sent) >= KEEPALIVE_MS {
            out.push(vec![T_PING]);
        }
        if !out.is_empty() {
            self.last_sent = now_ms;
        }
        Ok(out)
    }

    /// Доставленные сообщения (надёжные — по порядку; ненадёжные — по мере прихода).
    pub fn take_messages(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.delivered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    /// Двунаправленный канал с потерями, дублями и задержками (перестановка за счёт разных задержек).
    struct Link {
        rng: Lcg,
        loss: u64,
        dup: u64,
        in_flight: Vec<(u64, bool, Vec<u8>)>, // (время доставки, к b?, кадр)
    }

    impl Link {
        fn new(seed: u64, loss: u64, dup: u64) -> Self {
            Self {
                rng: Lcg(seed),
                loss,
                dup,
                in_flight: Vec::new(),
            }
        }
        fn transmit(&mut self, now: u64, to_b: bool, frames: Vec<Vec<u8>>) {
            for f in frames {
                if self.rng.chance(self.loss) {
                    continue;
                }
                let delay = 10 + self.rng.next() % 60; // 10..70 мс, из-за разброса кадры перемешиваются
                self.in_flight.push((now + delay, to_b, f.clone()));
                if self.rng.chance(self.dup) {
                    self.in_flight
                        .push((now + delay + self.rng.next() % 40, to_b, f));
                }
            }
        }
        fn deliverable(&mut self, now: u64) -> Vec<(bool, Vec<u8>)> {
            let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.in_flight)
                .into_iter()
                .partition(|(t, _, _)| *t <= now);
            self.in_flight = rest;
            let mut ready = ready;
            ready.sort_by_key(|(t, _, _)| *t);
            ready.into_iter().map(|(_, b, f)| (b, f)).collect()
        }
    }

    fn pump(
        a: &mut Channel,
        b: &mut Channel,
        link: &mut Link,
        now: u64,
    ) -> Result<(), ChannelError> {
        for (to_b, f) in link.deliverable(now) {
            if to_b {
                b.on_frame(now, &f)?
            } else {
                a.on_frame(now, &f)?
            }
        }
        let fa = a.poll(now)?;
        link.transmit(now, true, fa);
        let fb = b.poll(now)?;
        link.transmit(now, false, fb);
        Ok(())
    }

    fn run(a: &mut Channel, b: &mut Channel, link: &mut Link, from: u64, to: u64) -> u64 {
        let mut now = from;
        while now < to {
            pump(a, b, link, now).unwrap();
            now += 5;
        }
        now
    }

    #[test]
    fn small_and_large_messages_arrive_intact_and_in_order_on_a_perfect_link() {
        let (mut a, mut b, mut l) = (Channel::new(), Channel::new(), Link::new(1, 0, 0));
        let big: Vec<u8> = (0..60_000u32).map(|i| (i * 7 % 251) as u8).collect();
        a.send_reliable(b"first").unwrap();
        a.send_reliable(&big).unwrap();
        a.send_reliable(b"").unwrap();
        a.send_reliable(b"last").unwrap();
        run(&mut a, &mut b, &mut l, 0, 3000);
        let got = b.take_messages();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], b"first");
        assert_eq!(got[1], big);
        assert!(got[2].is_empty());
        assert_eq!(got[3], b"last");
        assert!(a.is_idle(), "всё подтверждено");
    }

    #[test]
    fn heavy_loss_duplication_and_reordering_still_deliver_everything_exactly_once_in_order() {
        for seed in 1..=5 {
            let (mut a, mut b, mut l) = (Channel::new(), Channel::new(), Link::new(seed, 35, 15));
            let mut rng = Lcg(seed * 99);
            let mut sent = Vec::new();
            for i in 0..120u32 {
                let len = if i % 17 == 0 {
                    20_000 + (rng.next() % 30_000) as usize
                } else {
                    (rng.next() % 1500) as usize
                };
                let msg: Vec<u8> = (0..len).map(|k| (k as u32 ^ i) as u8).collect();
                a.send_reliable(&msg).unwrap();
                sent.push(msg);
            }
            // Встречный поток: b тоже отправляет.
            for i in 0..30u8 {
                b.send_reliable(&[i; 300]).unwrap();
            }
            let mut got_b = Vec::new();
            let mut got_a = Vec::new();
            let mut now = 0;
            while now < 120_000 && (got_b.len() < sent.len() || got_a.len() < 30) {
                pump(&mut a, &mut b, &mut l, now).unwrap();
                got_b.extend(b.take_messages());
                got_a.extend(a.take_messages());
                now += 5;
            }
            assert_eq!(
                got_b.len(),
                sent.len(),
                "seed {seed}: доставлено {} из {}",
                got_b.len(),
                sent.len()
            );
            assert!(
                got_b.iter().zip(&sent).all(|(g, s)| g == s),
                "seed {seed}: порядок и содержимое"
            );
            assert_eq!(got_a.len(), 30);
            assert!(
                got_a
                    .iter()
                    .enumerate()
                    .all(|(i, m)| m == &vec![i as u8; 300])
            );
        }
    }

    #[test]
    fn unreliable_channel_drops_stale_and_duplicate_messages() {
        let (mut a, mut b) = (Channel::new(), Channel::new());
        for i in 0..5u8 {
            a.send_unreliable(&[i]).unwrap();
        }
        let frames = a.poll(0).unwrap();
        assert_eq!(frames.len(), 5);
        // Приходят в обратном порядке и с дублем: принимается только возрастающая последовательность.
        for f in [
            &frames[3], &frames[1], &frames[4], &frames[4], &frames[2], &frames[0],
        ] {
            b.on_frame(1, f).unwrap();
        }
        assert_eq!(b.take_messages(), vec![vec![3u8], vec![4u8]]);
        assert!(matches!(
            a.send_unreliable(&vec![0; MTU_PAYLOAD + 1]),
            Err(ChannelError::TooLarge(_))
        ));
    }

    #[test]
    fn limits_and_protocol_violations_are_errors_not_panics() {
        let mut c = Channel::new();
        assert!(matches!(
            c.send_reliable(&vec![0; MAX_MESSAGE_BYTES + 1]),
            Err(ChannelError::TooLarge(_))
        ));
        for bad in [
            &[][..],
            &[99],
            &[T_RELIABLE, 1, 2],
            &[T_ACK, 0, 0],
            &[T_UNRELIABLE, 1],
        ] {
            assert!(c.on_frame(0, bad).is_err(), "{bad:?}");
        }
        // Фрагмент с idx >= count и с count = 0.
        let frag = |seq: u32, idx: u16, count: u16| {
            let mut f = vec![T_RELIABLE];
            f.extend_from_slice(&seq.to_be_bytes());
            f.extend_from_slice(&idx.to_be_bytes());
            f.extend_from_slice(&count.to_be_bytes());
            f.push(1);
            f
        };
        assert!(c.on_frame(0, &frag(0, 5, 3)).is_err());
        assert!(c.on_frame(0, &frag(0, 0, 0)).is_err());
        assert!(c.on_frame(0, &frag(0, 0, u16::MAX)).is_err());
        // Нарушен порядок фрагментов: первым пришёл idx = 1.
        assert!(c.on_frame(0, &frag(0, 1, 2)).is_err());
        // Переполнение очереди.
        let mut d = Channel::new();
        let mut hit = false;
        for _ in 0..1000 {
            if d.send_reliable(&vec![0; MAX_MESSAGE_BYTES]).is_err() {
                hit = true;
                break;
            }
        }
        assert!(hit, "очередь ограничена");
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut rng = Lcg(77);
        let mut c = Channel::new();
        for i in 0..20_000u64 {
            let len = (rng.next() % 40) as usize;
            let frame: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let _ = c.on_frame(i, &frame);
            let _ = c.poll(i);
            c.take_messages();
        }
    }

    #[test]
    fn dead_peer_is_detected_and_keepalive_keeps_a_quiet_link_alive() {
        // Тихий, но живой канал: пинги и ответы не дают ему умереть.
        let (mut a, mut b, mut l) = (Channel::new(), Channel::new(), Link::new(3, 0, 0));
        run(&mut a, &mut b, &mut l, 0, 60_000);
        // Собеседник пропал: данные отправляются, но ничего не приходит.
        let mut dead = Channel::new();
        dead.send_reliable(b"hello?").unwrap();
        let mut err = None;
        for now in (0..30_000).step_by(10) {
            if let Err(e) = dead.poll(now) {
                err = Some((now, e));
                break;
            }
        }
        let (at, e) = err.expect("таймаут обнаружен");
        assert_eq!(e, ChannelError::Timeout);
        assert!(
            at >= PEER_TIMEOUT_MS - 100 || at > 5000,
            "не раньше разумного срока: {at}"
        );
    }

    #[test]
    fn retransmission_backs_off_and_acks_stop_it() {
        let mut a = Channel::new();
        a.send_reliable(b"x").unwrap();
        let first = a.poll(0).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            a.poll(100).unwrap().is_empty(),
            "до RTO ничего не повторяется"
        );
        assert_eq!(a.poll(150).unwrap().len(), 1, "первая переотправка");
        assert!(
            a.poll(400).unwrap().is_empty(),
            "следующая — не раньше удвоенного RTO"
        );
        assert_eq!(a.poll(450).unwrap().len(), 1);
        // Подтверждение останавливает переотправки.
        let mut ack = vec![T_ACK];
        ack.extend_from_slice(&1u32.to_be_bytes());
        ack.extend_from_slice(&0u64.to_be_bytes());
        a.on_frame(460, &ack).unwrap();
        assert!(a.is_idle());
        assert!(a.poll(5000).unwrap().iter().all(|f| f[0] != T_RELIABLE));
    }

    #[test]
    fn sequence_numbers_survive_wraparound() {
        let mut a = Channel {
            next_seq: u32::MAX - 2,
            ..Channel::new()
        };
        let mut b = Channel {
            recv_next: u32::MAX - 2,
            ..Channel::new()
        };
        let mut l = Link::new(9, 20, 10);
        for i in 0..10u8 {
            a.send_reliable(&[i; 50]).unwrap();
        }
        let mut got = Vec::new();
        let mut now = 0;
        while now < 20_000 && got.len() < 10 {
            pump(&mut a, &mut b, &mut l, now).unwrap();
            got.extend(b.take_messages());
            now += 5;
        }
        assert_eq!(got.len(), 10);
        assert!(got.iter().enumerate().all(|(i, m)| m == &vec![i as u8; 50]));
    }
}
