//! Сообщения между клиентом и сервером. Кодирование — `postcard`.

use astrocore_ecs::GameTick;
use serde::{Deserialize, Serialize};

use crate::ids::NetEntity;

/// Наибольший размер состояния для ненадёжного канала: помещается в один пакет UDP.
pub const MAX_UNRELIABLE_BYTES: usize = 1100;
/// Размер части большого состояния в надёжном канале.
pub const STATE_PART_BYTES: usize = 16 * 1024;
/// Наибольшее число частей одного состояния (64 МиБ).
pub const MAX_STATE_PARTS: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Первое сообщение: имя игрока и отпечаток набора сетевых компонентов.
    Hello { name: String, fingerprint: u64 },
    /// Клиент применил состояние на этот тик.
    Ack(GameTick),
    /// Клиент потерял согласованность и просит мир заново.
    RequestFull,
    /// Сетевое событие.
    Event { id: u16, bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMessage {
    Welcome {
        tick_rate: u16,
        tick: GameTick,
    },
    Rejected(String),
    State(GameState),
    /// Часть большого состояния: после сборки — закодированное `ServerMessage::State`.
    StatePart {
        id: u32,
        index: u32,
        count: u32,
        bytes: Vec<u8>,
    },
    /// Сущность, которой управляет игрок.
    Attached(Option<NetEntity>),
    /// Сетевое событие.
    Event {
        id: u16,
        bytes: Vec<u8>,
    },
    /// Сжатое состояние: после распаковки — закодированное `ServerMessage::State`.
    Packed {
        /// Размер после распаковки.
        size: u32,
        bytes: Vec<u8>,
    },
}

/// Наибольший размер состояния после распаковки: столько же, сколько вмещают части.
pub const MAX_STATE_BYTES: usize = MAX_STATE_PARTS as usize * STATE_PART_BYTES;

/// Кодирует состояние для отправки. Состояние, которое не помещается в один пакет,
/// сжимается (LZ4): вход в игру и большие изменения состоят из повторяющихся данных —
/// тайлов и одинаковых компонентов.
pub fn encode_state(state: GameState) -> Vec<u8> {
    let raw = postcard::to_allocvec(&ServerMessage::State(state)).unwrap_or_default();
    if raw.len() <= MAX_UNRELIABLE_BYTES {
        return raw;
    }
    let packed = ServerMessage::Packed {
        size: raw.len() as u32,
        bytes: lz4_flex::block::compress(&raw),
    };
    match postcard::to_allocvec(&packed) {
        Ok(message) if message.len() < raw.len() => message,
        _ => raw,
    }
}

/// Состояние из сообщения сервера: обычное или сжатое. `None` — сообщение не состояние
/// или сжатые данные испорчены.
pub fn decode_state(message: ServerMessage) -> Option<GameState> {
    match message {
        ServerMessage::State(state) => Some(state),
        ServerMessage::Packed { size, bytes } => {
            // Размер приходит от сервера, но память под него выделяет клиент.
            if size as usize > MAX_STATE_BYTES {
                return None;
            }
            let raw = lz4_flex::block::decompress(&bytes, size as usize).ok()?;
            match postcard::from_bytes(&raw).ok()? {
                ServerMessage::State(state) => Some(state),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Состояние мира для одного клиента (`GameState` Robust).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GameState {
    /// Тик состояния, подтверждённого клиентом, от которого построено это.
    pub from: GameTick,
    /// Тик этого состояния.
    pub to: GameTick,
    /// Номер последнего кадра ввода этого клиента, который сервер обработал.
    pub last_input: u32,
    /// Сущности, удалённые после `from`.
    pub deleted: Vec<NetEntity>,
    /// Сущности, вышедшие из зоны видимости.
    pub left: Vec<NetEntity>,
    /// Новые и изменившиеся сущности: родитель раньше потомка.
    pub entities: Vec<EntityState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityState {
    pub net: NetEntity,
    /// `Some` — сущность новая для клиента; внутри — её прототип.
    pub prototype: Option<Option<String>>,
    /// Сетевые компоненты сущности: приходит для новой сущности и после удаления компонента.
    pub present: Option<Vec<u16>>,
    /// Состояния компонентов: сетевой номер и данные.
    pub components: Vec<(u16, Vec<u8>)>,
}
