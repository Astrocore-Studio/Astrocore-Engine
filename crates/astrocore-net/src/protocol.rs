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
}

/// Состояние мира для одного клиента (`GameState` Robust).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GameState {
    /// Тик состояния, подтверждённого клиентом, от которого построено это.
    pub from: GameTick,
    /// Тик этого состояния.
    pub to: GameTick,
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
