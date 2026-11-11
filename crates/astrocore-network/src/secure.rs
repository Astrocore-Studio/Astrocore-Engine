use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const MAGIC_HELLO1: u8 = 0xA1;
pub const MAGIC_HELLO2: u8 = 0xA2;
/// Минимальный размер первого пакета клиента: ответ сервера не длиннее запроса (нет усиления).
pub const HELLO1_MIN_LEN: usize = 96;
const HELLO2_LEN: usize = 1 + 32 + 16;
const TAG_LEN: usize = 16;
pub const REPLAY_WINDOW: u64 = 128;
/// Предел числа записей на ключ (далеко до переполнения nonce, но с запасом на безопасность).
pub const MAX_RECORDS: u64 = 1 << 40;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecureError {
    #[error("неверный формат рукопожатия")]
    BadHandshake,
    #[error("сервер не подтвердил знание ключа (подмена или неверный ключ)")]
    ServerNotAuthenticated,
    #[error("недопустимый открытый ключ")]
    BadPublicKey,
    #[error("запись не прошла проверку подлинности")]
    BadRecord,
    #[error("повторная или устаревшая запись")]
    Replay,
    #[error("исчерпан ресурс ключа")]
    KeyExhausted,
    #[error("нет случайных данных от ОС: {0}")]
    Random(String),
}

fn random32() -> Result<[u8; 32], SecureError> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| SecureError::Random(e.to_string()))?;
    Ok(b)
}

/// Долговременная пара ключей сервера.
#[derive(Clone)]
pub struct StaticKeypair {
    secret: StaticSecret,
    public: [u8; 32],
}

impl StaticKeypair {
    pub fn generate() -> Result<Self, SecureError> {
        Ok(Self::from_secret_bytes(random32()?))
    }

    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }
}

impl std::fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticKeypair")
            .field("public", &hex(&self.public))
            .finish_non_exhaustive()
    }
}

/// Загрузить долговременный ключ сервера из файла (64 hex-символа секрета) или создать новый.
/// Файл создаётся с правами только для владельца (Unix).
pub fn load_or_create_keypair(path: &std::path::Path) -> Result<StaticKeypair, SecureError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_hex32(&text)
            .map(StaticKeypair::from_secret_bytes)
            .ok_or(SecureError::BadHandshake),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let keys = StaticKeypair::generate()?;
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| SecureError::Random(e.to_string()))?;
            }
            write_private(path, hex(&keys.secret_bytes()).as_bytes())
                .map_err(|e| SecureError::Random(e.to_string()))?;
            Ok(keys)
        }
        Err(e) => Err(SecureError::Random(e.to_string())),
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn dh(secret: &StaticSecret, public: &[u8; 32]) -> Result<[u8; 32], SecureError> {
    let shared = secret.diffie_hellman(&PublicKey::from(*public));
    if !shared.was_contributory() {
        return Err(SecureError::BadPublicKey); // точка малого порядка
    }
    Ok(shared.to_bytes())
}

struct Keys {
    c2s: [u8; 32],
    s2c: [u8; 32],
}

fn derive(es: &[u8; 32], ee: &[u8; 32], e_c: &[u8; 32], e_s: &[u8; 32], s_pub: &[u8; 32]) -> Keys {
    let mut transcript = Sha256::new();
    transcript.update(b"astrocore-secure-v1");
    for part in [e_c, e_s, s_pub] {
        transcript.update(part);
    }
    let salt = transcript.finalize();
    let mut ikm = Vec::with_capacity(64);
    ikm.extend_from_slice(es);
    ikm.extend_from_slice(ee);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let (mut c2s, mut s2c) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"astrocore c2s", &mut c2s)
        .expect("32 байта допустимы для HKDF-SHA256");
    hk.expand(b"astrocore s2c", &mut s2c)
        .expect("32 байта допустимы для HKDF-SHA256");
    Keys { c2s, s2c }
}

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    *Nonce::from_slice(&n)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    ClientToServer = 1,
    ServerToClient = 2,
}

/// Состояние защищённой сессии одной стороны.
pub struct SecureSession {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_dir: Dir,
    next_counter: u64,
    /// Наибольший принятый номер и битовая маска предыдущих `REPLAY_WINDOW`.
    highest: Option<u64>,
    seen: u128,
}

impl std::fmt::Debug for SecureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureSession")
            .field("sent", &self.next_counter)
            .finish_non_exhaustive()
    }
}

impl SecureSession {
    fn new(keys: &Keys, is_client: bool) -> Self {
        let (send_key, recv_key, send_dir) = if is_client {
            (keys.c2s, keys.s2c, Dir::ClientToServer)
        } else {
            (keys.s2c, keys.c2s, Dir::ServerToClient)
        };
        Self {
            send: ChaCha20Poly1305::new(Key::from_slice(&send_key)),
            recv: ChaCha20Poly1305::new(Key::from_slice(&recv_key)),
            send_dir,
            next_counter: 1, // 0 занят подтверждением рукопожатия
            highest: None,
            seen: 0,
        }
    }

    fn aad(dir: Dir, counter: u64) -> [u8; 9] {
        let mut a = [0u8; 9];
        a[0] = dir as u8;
        a[1..].copy_from_slice(&counter.to_be_bytes());
        a
    }

    /// Зашифровать: `[счётчик u64 BE][шифртекст + тег]`.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, SecureError> {
        if self.next_counter >= MAX_RECORDS {
            return Err(SecureError::KeyExhausted);
        }
        let counter = self.next_counter;
        self.next_counter += 1;
        let ct = self
            .send
            .encrypt(
                &nonce(counter),
                Payload {
                    msg: plaintext,
                    aad: &Self::aad(self.send_dir, counter),
                },
            )
            .map_err(|_| SecureError::BadRecord)?;
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Проверить и расшифровать запись. Состояние окна повторов меняется **только** после
    /// успешной проверки подлинности — подделка не сдвигает окно.
    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, SecureError> {
        if record.len() < 8 + TAG_LEN {
            return Err(SecureError::BadRecord);
        }
        let counter = u64::from_be_bytes(record[..8].try_into().expect("длина проверена"));
        if counter == 0 || counter >= MAX_RECORDS {
            return Err(SecureError::BadRecord);
        }
        // Предварительная проверка окна (дёшево), затем расшифровка, затем фиксация.
        if let Some(h) = self.highest
            && counter <= h
        {
            let back = h - counter;
            if back >= REPLAY_WINDOW || self.seen & (1u128 << back) != 0 {
                return Err(SecureError::Replay);
            }
        }
        let recv_dir = if self.send_dir == Dir::ClientToServer {
            Dir::ServerToClient
        } else {
            Dir::ClientToServer
        };
        let pt = self
            .recv
            .decrypt(
                &nonce(counter),
                Payload {
                    msg: &record[8..],
                    aad: &Self::aad(recv_dir, counter),
                },
            )
            .map_err(|_| SecureError::BadRecord)?;
        match self.highest {
            Some(h) if counter <= h => self.seen |= 1u128 << (h - counter),
            Some(h) => {
                let shift = counter - h;
                self.seen = if shift >= 128 { 0 } else { self.seen << shift };
                self.seen |= 1; // бит 0 — сам `highest`
                self.highest = Some(counter);
            }
            None => {
                self.highest = Some(counter);
                self.seen = 1;
            }
        }
        Ok(pt)
    }
}

/// Клиентская половина рукопожатия.
pub struct ClientHandshake {
    e_secret: StaticSecret,
    e_pub: [u8; 32],
    server_pub: [u8; 32],
}

/// Начать рукопожатие. Возвращает состояние и первый пакет (дополнен до [`HELLO1_MIN_LEN`]).
pub fn client_start(server_public: [u8; 32]) -> Result<(ClientHandshake, Vec<u8>), SecureError> {
    client_start_with(server_public, random32()?)
}

/// То же с заданным эфемерным секретом (для воспроизводимых тестов).
pub fn client_start_with(
    server_public: [u8; 32],
    ephemeral_secret: [u8; 32],
) -> Result<(ClientHandshake, Vec<u8>), SecureError> {
    let e_secret = StaticSecret::from(ephemeral_secret);
    let e_pub = PublicKey::from(&e_secret).to_bytes();
    let mut hello = vec![0u8; HELLO1_MIN_LEN];
    hello[0] = MAGIC_HELLO1;
    hello[1..33].copy_from_slice(&e_pub);
    Ok((
        ClientHandshake {
            e_secret,
            e_pub,
            server_pub: server_public,
        },
        hello,
    ))
}

impl ClientHandshake {
    /// Принять ответ сервера. Подтверждение проверяется ключом, в который вошёл DH с
    /// долговременным ключом сервера: без закрытого ключа сервера его не подделать.
    pub fn finish(self, hello2: &[u8]) -> Result<SecureSession, SecureError> {
        if hello2.len() != HELLO2_LEN || hello2[0] != MAGIC_HELLO2 {
            return Err(SecureError::BadHandshake);
        }
        let e_s: [u8; 32] = hello2[1..33].try_into().expect("длина проверена");
        let es = dh(&self.e_secret, &self.server_pub)?;
        let ee = dh(&self.e_secret, &e_s)?;
        let keys = derive(&es, &ee, &self.e_pub, &e_s, &self.server_pub);
        let confirm = ChaCha20Poly1305::new(Key::from_slice(&keys.s2c));
        confirm
            .decrypt(
                &nonce(0),
                Payload {
                    msg: &hello2[33..],
                    aad: &SecureSession::aad(Dir::ServerToClient, 0),
                },
            )
            .map_err(|_| SecureError::ServerNotAuthenticated)?;
        Ok(SecureSession::new(&keys, true))
    }
}

/// Серверная половина: принять первый пакет, вернуть сессию и ответ.
pub fn server_accept(
    static_keys: &StaticKeypair,
    hello1: &[u8],
) -> Result<(SecureSession, Vec<u8>), SecureError> {
    server_accept_with(static_keys, hello1, random32()?)
}

pub fn server_accept_with(
    static_keys: &StaticKeypair,
    hello1: &[u8],
    ephemeral_secret: [u8; 32],
) -> Result<(SecureSession, Vec<u8>), SecureError> {
    if hello1.len() < HELLO1_MIN_LEN || hello1[0] != MAGIC_HELLO1 {
        return Err(SecureError::BadHandshake);
    }
    let e_c: [u8; 32] = hello1[1..33].try_into().expect("длина проверена");
    let e_secret = StaticSecret::from(ephemeral_secret);
    let e_s = PublicKey::from(&e_secret).to_bytes();
    let es = dh(&static_keys.secret, &e_c)?;
    let ee = dh(&e_secret, &e_c)?;
    let keys = derive(&es, &ee, &e_c, &e_s, &static_keys.public);
    let confirm = ChaCha20Poly1305::new(Key::from_slice(&keys.s2c));
    let tag = confirm
        .encrypt(
            &nonce(0),
            Payload {
                msg: &[],
                aad: &SecureSession::aad(Dir::ServerToClient, 0),
            },
        )
        .map_err(|_| SecureError::BadHandshake)?;
    let mut hello2 = Vec::with_capacity(HELLO2_LEN);
    hello2.push(MAGIC_HELLO2);
    hello2.extend_from_slice(&e_s);
    hello2.extend_from_slice(&tag);
    Ok((SecureSession::new(&keys, false), hello2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (SecureSession, SecureSession, StaticKeypair) {
        let server_keys = StaticKeypair::generate().unwrap();
        let (hs, hello1) = client_start(server_keys.public()).unwrap();
        let (server, hello2) = server_accept(&server_keys, &hello1).unwrap();
        let client = hs.finish(&hello2).unwrap();
        (client, server, server_keys)
    }

    #[test]
    fn handshake_and_bidirectional_records() {
        let (mut c, mut s, _) = pair();
        let r = c.seal(b"hello server").unwrap();
        assert_eq!(s.open(&r).unwrap(), b"hello server");
        let r = s.seal(b"hello client").unwrap();
        assert_eq!(c.open(&r).unwrap(), b"hello client");
        assert!(
            !r.windows(5).any(|w| w == b"hello"),
            "в записи нет открытого текста"
        );
        assert_eq!(r.len(), 8 + b"hello client".len() + TAG_LEN);
    }

    #[test]
    fn hello1_is_padded_so_the_reply_never_amplifies() {
        let keys = StaticKeypair::generate().unwrap();
        let (_, hello1) = client_start(keys.public()).unwrap();
        let (_, hello2) = server_accept(&keys, &hello1).unwrap();
        assert!(hello1.len() >= HELLO1_MIN_LEN && hello2.len() < hello1.len());
        assert!(matches!(
            server_accept(&keys, &hello1[..40]),
            Err(SecureError::BadHandshake)
        ));
    }

    #[test]
    fn wrong_server_key_or_impostor_is_detected() {
        let real = StaticKeypair::generate().unwrap();
        let impostor = StaticKeypair::generate().unwrap();
        // Клиент закрепил ключ настоящего сервера, но отвечает самозванец.
        let (hs, hello1) = client_start(real.public()).unwrap();
        let (_, fake_hello2) = server_accept(&impostor, &hello1).unwrap();
        assert!(matches!(
            hs.finish(&fake_hello2),
            Err(SecureError::ServerNotAuthenticated)
        ));
        // Мусорный ответ.
        let (hs, _) = client_start(real.public()).unwrap();
        assert!(matches!(
            hs.finish(&[0u8; 49]),
            Err(SecureError::BadHandshake)
        ));
        let (hs, _) = client_start(real.public()).unwrap();
        assert!(matches!(
            hs.finish(&[MAGIC_HELLO2; 10]),
            Err(SecureError::BadHandshake)
        ));
    }

    #[test]
    fn low_order_public_keys_are_rejected() {
        let keys = StaticKeypair::generate().unwrap();
        let mut hello1 = vec![0u8; HELLO1_MIN_LEN];
        hello1[0] = MAGIC_HELLO1; // эфемерный ключ клиента = нулевая точка
        assert!(matches!(
            server_accept(&keys, &hello1),
            Err(SecureError::BadPublicKey)
        ));
        let (hs, _) = client_start(keys.public()).unwrap();
        let mut hello2 = vec![0u8; HELLO2_LEN];
        hello2[0] = MAGIC_HELLO2;
        assert!(matches!(hs.finish(&hello2), Err(SecureError::BadPublicKey)));
    }

    #[test]
    fn tampering_and_replays_are_refused_without_corrupting_state() {
        let (mut c, mut s, _) = pair();
        let r1 = c.seal(b"one").unwrap();
        let r2 = c.seal(b"two").unwrap();
        // Подделка: изменённый байт, усечение, мусор.
        let mut bad = r1.clone();
        bad[10] ^= 1;
        assert_eq!(s.open(&bad), Err(SecureError::BadRecord));
        assert_eq!(s.open(&r1[..r1.len() - 1]), Err(SecureError::BadRecord));
        assert_eq!(s.open(&[1, 2, 3]), Err(SecureError::BadRecord));
        // Подделка не сдвинула окно: настоящая запись по-прежнему проходит.
        assert_eq!(s.open(&r1).unwrap(), b"one");
        assert_eq!(s.open(&r1), Err(SecureError::Replay));
        assert_eq!(s.open(&r2).unwrap(), b"two");
        // Запись, направленная «не туда» (из s2c в c2s), не проходит: направление входит в AAD и ключ.
        let back = s.seal(b"x").unwrap();
        assert!(s.open(&back).is_err());
        // Счётчик 0 зарезервирован.
        let mut zero = r2.clone();
        zero[..8].copy_from_slice(&0u64.to_be_bytes());
        assert_eq!(s.open(&zero), Err(SecureError::BadRecord));
    }

    #[test]
    fn datagram_reordering_within_the_window_is_allowed_but_not_beyond() {
        let (mut c, mut s, _) = pair();
        let recs: Vec<_> = (0..300u32)
            .map(|i| c.seal(&i.to_le_bytes()).unwrap())
            .collect();
        // Перестановка в окне.
        for i in [5usize, 3, 4, 1, 2, 0] {
            assert_eq!(s.open(&recs[i]).unwrap(), (i as u32).to_le_bytes());
        }
        for r in &recs[6..250] {
            s.open(r).unwrap();
        }
        // Слишком старая запись (за окном повторов) отбрасывается.
        assert_eq!(s.open(&recs[10]), Err(SecureError::Replay));
        // А внутри окна — ещё не виденная — проходит.
        let (mut c2, mut s2, _) = pair();
        let recs: Vec<_> = (0..200u32)
            .map(|i| c2.seal(&i.to_le_bytes()).unwrap())
            .collect();
        s2.open(&recs[199]).unwrap();
        assert!(s2.open(&recs[100]).is_ok(), "в пределах 128 назад");
        assert_eq!(s2.open(&recs[10]), Err(SecureError::Replay));
    }

    #[test]
    fn sessions_are_independent_and_keys_differ_between_handshakes() {
        let keys = StaticKeypair::generate().unwrap();
        let mk = || {
            let (hs, h1) = client_start(keys.public()).unwrap();
            let (s, h2) = server_accept(&keys, &h1).unwrap();
            (hs.finish(&h2).unwrap(), s)
        };
        let (mut c1, _s1) = mk();
        let (_c2, mut s2) = mk();
        let r = c1.seal(b"cross").unwrap();
        assert_eq!(
            s2.open(&r),
            Err(SecureError::BadRecord),
            "запись одной сессии не открывается другой"
        );
    }

    #[test]
    fn deterministic_handshake_matches_between_runs() {
        let keys = StaticKeypair::from_secret_bytes([7; 32]);
        let run = || {
            let (hs, h1) = client_start_with(keys.public(), [1; 32]).unwrap();
            let (mut s, h2) = server_accept_with(&keys, &h1, [2; 32]).unwrap();
            let mut c = hs.finish(&h2).unwrap();
            (h2, c.seal(b"x").unwrap(), s.seal(b"y").unwrap())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn key_files_are_created_private_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("acs-key-{}", std::process::id()));
        let path = dir.join("nested").join("server.key");
        let a = load_or_create_keypair(&path).unwrap();
        let b = load_or_create_keypair(&path).unwrap();
        assert_eq!(a.public(), b.public(), "тот же ключ при повторной загрузке");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, "не ключ").unwrap();
        assert!(
            load_or_create_keypair(&path).is_err(),
            "повреждённый файл не перезаписывается молча"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hex_roundtrip() {
        let k = StaticKeypair::generate().unwrap().public();
        assert_eq!(parse_hex32(&hex(&k)), Some(k));
        assert_eq!(parse_hex32("zz"), None);
        assert_eq!(parse_hex32(&"é".repeat(32)), None);
    }
}
