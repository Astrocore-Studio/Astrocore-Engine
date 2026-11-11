use std::collections::BTreeMap;

/// Надёжность доставки значений этого вида.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationChannel {
    /// Важные изменения: порядок и доставка гарантированы.
    ReliableOrdered,
    /// Частые изменения: устаревшие значения отбрасываются, нужна только последняя версия.
    UnreliableSequenced,
}

/// Кому показывать значения вида.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interest {
    /// Всем подключённым.
    Everyone,
    /// Тем, у кого сущность в зоне интереса (радиус в метрах).
    Radius(f32),
}

/// Описание реплицируемого вида данных, которое регистрирует игровой модуль.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedKind {
    pub name: String,
    pub channel: ReplicationChannel,
    pub interest: Interest,
    /// Предел размера значения, байт.
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReplicationError {
    #[error("вид {0:?} уже зарегистрирован")]
    Duplicate(String),
    #[error("неверное имя вида {0:?}: нужны 1..=64 символа [a-z0-9_.:-]")]
    BadName(String),
    #[error("неверный предел размера: {0}")]
    BadLimit(usize),
    #[error("вид {0:?} не зарегистрирован")]
    Unknown(String),
    #[error("значение вида {kind:?} слишком велико: {len} > {max}")]
    TooLarge {
        kind: String,
        len: usize,
        max: usize,
    },
}

/// Максимальный размер значения любого вида: ограничено лимитом пакета протокола.
pub const MAX_REPLICATED_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplicationRegistry {
    kinds: BTreeMap<String, ReplicatedKind>,
}

impl ReplicationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, kind: ReplicatedKind) -> Result<(), ReplicationError> {
        let ok_name = !kind.name.is_empty()
            && kind.name.len() <= 64
            && kind
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:-".contains(&b));
        if !ok_name {
            return Err(ReplicationError::BadName(kind.name));
        }
        if kind.max_bytes == 0 || kind.max_bytes > MAX_REPLICATED_BYTES {
            return Err(ReplicationError::BadLimit(kind.max_bytes));
        }
        if self.kinds.contains_key(&kind.name) {
            return Err(ReplicationError::Duplicate(kind.name));
        }
        self.kinds.insert(kind.name.clone(), kind);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ReplicatedKind> {
        self.kinds.get(name)
    }

    pub fn kinds(&self) -> impl Iterator<Item = &ReplicatedKind> {
        self.kinds.values()
    }

    /// Проверить значение перед отправкой или после приёма.
    pub fn check(&self, name: &str, len: usize) -> Result<&ReplicatedKind, ReplicationError> {
        let kind = self
            .kinds
            .get(name)
            .ok_or_else(|| ReplicationError::Unknown(name.to_string()))?;
        if len > kind.max_bytes {
            return Err(ReplicationError::TooLarge {
                kind: name.to_string(),
                len,
                max: kind.max_bytes,
            });
        }
        Ok(kind)
    }
}
