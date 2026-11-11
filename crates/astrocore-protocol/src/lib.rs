use astrocore_core::NetworkEntityId;
use astrocore_world::{WorldCommand, WorldOp};
use serde::{Deserialize, Serialize};

/// Версия протокола. Меняется при любом несовместимом изменении сообщений.
pub const PROTOCOL_VERSION: u16 = 1;
/// Максимальный размер пакета (байты) — отбрасывается до разбора.
pub const MAX_PACKET_BYTES: usize = 64 * 1024;
pub const MAX_BUILD_LEN: usize = 64;
pub const MAX_WORLD_ID_LEN: usize = 64;
pub const MAX_REASON_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Первое сообщение соединения: версии и идентификатор карты.
    Hello {
        build: String,
        world_id: String,
        world_hash: u64,
    },
    /// Ввод движения. Оси — `-1..=1`; `yaw` — радианы вокруг +Y.
    Input {
        seq: u32,
        move_x: i8,
        move_z: i8,
        yaw: f32,
    },
    /// Взаимодействие с сущностью (дверь и т. п.).
    Interact { target: NetworkEntityId },
    /// Запрос структурной правки мира; сервер проверяет права и ревизию.
    Edit {
        base_revision: u64,
        command: WorldCommand,
    },
    /// Клиент потерял состояние и просит базовый snapshot.
    RequestSnapshot,
    /// Правка рельефа (postcard-байты `TerrainEdit`); сервер проверяет права и ревизию рельефа.
    EditTerrain { base_revision: u64, data: Vec<u8> },
    /// Запросить содержимое по SHA-256 (ассеты и пакеты доставляются отдельно от игрового потока).
    RequestContent { hash: [u8; 32] },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Принятие соединения: частота тика, ревизия мира в момент входа и сетевой ID игрока.
    Welcome {
        tick_rate_hz: u8,
        revision: u64,
        you: NetworkEntityId,
    },
    /// Часть снимка мира (postcard-байты `WorldState`, разбитые на куски).
    SnapshotPart {
        index: u16,
        total: u16,
        data: Vec<u8>,
    },
    /// Подтверждённая операция над миром — применять строго по порядку `seq`.
    WorldOp(WorldOp),
    /// Часть снимка рельефа (postcard-байты `TerrainState`, разбитые на куски).
    TerrainSnapshotPart {
        index: u16,
        total: u16,
        data: Vec<u8>,
    },
    /// Подтверждённая правка рельефа (postcard-байты `TerrainEdit`), `seq` — ревизия после неё.
    TerrainOp { seq: u64, data: Vec<u8> },
    /// Подтверждение ввода и авторитетная позиция после его применения.
    InputAck { seq: u32, pos: [f32; 3] },
    /// Состояние видимой сущности (игрока). `tick` — серверный тик.
    EntityState {
        id: NetworkEntityId,
        pos: [f32; 3],
        yaw: f32,
        tick: u32,
    },
    /// Сущность вышла из зоны интереса или отключилась.
    EntityRemoved { id: NetworkEntityId },
    /// Состояние двери. `net` — сетевой ID, который клиент использует в `Interact`.
    DoorState {
        net: NetworkEntityId,
        element: astrocore_core::ObjectId,
        open: bool,
    },
    /// Значение зарегистрированного вида данных, привязанное к сущности (репликация игровых
    /// модулей). `seq` растёт с каждым изменением вида у сущности; клиент отбрасывает старые.
    ComponentDelta {
        net: NetworkEntityId,
        kind: String,
        seq: u32,
        /// Доставлять надёжно (иначе — последнее значение важнее порядка).
        reliable: bool,
        data: Vec<u8>,
    },
    /// Часть запрошенного содержимого; целостность проверяется клиентом по `hash`.
    ContentPart {
        hash: [u8; 32],
        index: u32,
        total: u32,
        data: Vec<u8>,
    },
    /// Содержимого с таким хешем у сервера нет.
    ContentMissing { hash: [u8; 32] },
    Reject {
        reason: RejectReason,
        detail: String,
    },
}

/// Размер части содержимого и предел числа частей (64 МиБ на объект).
pub const CONTENT_PART_BYTES: usize = 16 * 1024;
pub const MAX_CONTENT_PARTS: u32 = 4096;

/// Предел длины имени вида и размера значения в `ComponentDelta`.
pub const MAX_COMPONENT_KIND_LEN: usize = 64;
pub const MAX_COMPONENT_BYTES: usize = 16 * 1024;

/// Максимальный размер одного куска снимка, байты.
pub const MAX_SNAPSHOT_PART_BYTES: usize = 48 * 1024;
/// Максимальное число кусков снимка (≈ 192 МиБ).
pub const MAX_SNAPSHOT_PARTS: u16 = 4096;
/// Максимальный размер правки рельефа в сообщении, байты.
pub const MAX_TERRAIN_EDIT_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    IncompatibleProtocol,
    IncompatibleWorld,
    NotAuthorized,
    StaleRevision,
    InvalidCommand,
    RateLimited,
    ServerFull,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("пакет слишком велик: {0} байт")]
    TooLarge(usize),
    #[error("пакет слишком короткий")]
    Truncated,
    #[error("несовместимая версия протокола: {got} (ожидалась {PROTOCOL_VERSION})")]
    VersionMismatch { got: u16 },
    #[error("ошибка разбора: {0}")]
    Decode(String),
    #[error("после сообщения остались лишние байты ({0})")]
    TrailingBytes(usize),
    #[error("недопустимое значение: {0}")]
    Invalid(&'static str),
    #[error("ошибка кодирования: {0}")]
    Encode(String),
}

/// Сообщения, которые можно проверить на смысл после разбора.
pub trait Validate {
    fn validate(&self) -> Result<(), ProtocolError>;
}

impl Validate for ClientMessage {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            ClientMessage::Hello {
                build, world_id, ..
            } => {
                if build.len() > MAX_BUILD_LEN || build.chars().any(char::is_control) {
                    return Err(ProtocolError::Invalid("build"));
                }
                if world_id.len() > MAX_WORLD_ID_LEN || world_id.chars().any(char::is_control) {
                    return Err(ProtocolError::Invalid("world_id"));
                }
            }
            ClientMessage::Input {
                move_x,
                move_z,
                yaw,
                ..
            } => {
                if !(-1..=1).contains(move_x) || !(-1..=1).contains(move_z) {
                    return Err(ProtocolError::Invalid("move axis"));
                }
                if !yaw.is_finite() {
                    return Err(ProtocolError::Invalid("yaw"));
                }
            }
            ClientMessage::EditTerrain { data, .. } if data.len() > MAX_TERRAIN_EDIT_BYTES => {
                return Err(ProtocolError::Invalid("terrain edit size"));
            }
            ClientMessage::Interact { .. }
            | ClientMessage::RequestSnapshot
            | ClientMessage::RequestContent { .. }
            | ClientMessage::EditTerrain { .. } => {}
            ClientMessage::Edit { .. } => {}
        }
        Ok(())
    }
}

impl Validate for ServerMessage {
    fn validate(&self) -> Result<(), ProtocolError> {
        let finite = |p: &[f32; 3]| p.iter().all(|v| v.is_finite());
        match self {
            ServerMessage::Reject { detail, .. } if detail.len() > MAX_REASON_LEN => {
                Err(ProtocolError::Invalid("reject detail"))
            }
            ServerMessage::SnapshotPart { index, total, data }
            | ServerMessage::TerrainSnapshotPart { index, total, data } => {
                if *total == 0 || *total > MAX_SNAPSHOT_PARTS || index >= total {
                    Err(ProtocolError::Invalid("snapshot index/total"))
                } else if data.len() > MAX_SNAPSHOT_PART_BYTES {
                    Err(ProtocolError::Invalid("snapshot part size"))
                } else {
                    Ok(())
                }
            }
            ServerMessage::TerrainOp { data, .. } if data.len() > MAX_TERRAIN_EDIT_BYTES => {
                Err(ProtocolError::Invalid("terrain op size"))
            }
            ServerMessage::ComponentDelta { kind, data, .. }
                if kind.is_empty()
                    || kind.len() > MAX_COMPONENT_KIND_LEN
                    || kind.chars().any(char::is_control)
                    || data.len() > MAX_COMPONENT_BYTES =>
            {
                Err(ProtocolError::Invalid("component delta"))
            }
            ServerMessage::ContentPart {
                index, total, data, ..
            } if *total == 0
                || *total > MAX_CONTENT_PARTS
                || index >= total
                || data.len() > CONTENT_PART_BYTES =>
            {
                Err(ProtocolError::Invalid("content part"))
            }
            ServerMessage::InputAck { pos, .. } if !finite(pos) => {
                Err(ProtocolError::Invalid("pos"))
            }
            ServerMessage::EntityState { pos, yaw, .. } if !finite(pos) || !yaw.is_finite() => {
                Err(ProtocolError::Invalid("entity state"))
            }
            _ => Ok(()),
        }
    }
}

/// Закодировать сообщение: `[версия: u16 LE][тело postcard]`.
pub fn encode<T: Serialize + Validate>(msg: &T) -> Result<Vec<u8>, ProtocolError> {
    msg.validate()?;
    let mut out = PROTOCOL_VERSION.to_le_bytes().to_vec();
    let body = postcard::to_stdvec(msg).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    out.extend_from_slice(&body);
    if out.len() > MAX_PACKET_BYTES {
        return Err(ProtocolError::TooLarge(out.len()));
    }
    Ok(out)
}

/// Разобрать недоверенный пакет. Никогда не паникует.
pub fn decode<T: for<'de> Deserialize<'de> + Validate>(packet: &[u8]) -> Result<T, ProtocolError> {
    if packet.len() > MAX_PACKET_BYTES {
        return Err(ProtocolError::TooLarge(packet.len()));
    }
    let (version, body) = packet
        .split_first_chunk::<2>()
        .ok_or(ProtocolError::Truncated)?;
    let version = u16::from_le_bytes(*version);
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch { got: version });
    }
    let (msg, rest) =
        postcard::take_from_bytes::<T>(body).map_err(|e| ProtocolError::Decode(e.to_string()))?;
    if !rest.is_empty() {
        return Err(ProtocolError::TrailingBytes(rest.len()));
    }
    msg.validate()?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrocore_core::{ObjectId, PrototypeId};
    use astrocore_math::GridEdge;
    use astrocore_world::{FloorId, Placement, StructureElement, StructureKind};

    fn hello() -> ClientMessage {
        ClientMessage::Hello {
            build: "dev".into(),
            world_id: "w".into(),
            world_hash: 0xDEAD_BEEF,
        }
    }

    fn edit() -> ClientMessage {
        ClientMessage::Edit {
            base_revision: 5,
            command: WorldCommand::PlaceElement(StructureElement {
                id: ObjectId::from_u128(1),
                kind: StructureKind::Wall,
                placement: Placement::Edge(GridEdge::along_x(1, 2)),
                floor: FloorId(0),
                material: PrototypeId::new("t:m").unwrap(),
                thickness: 0.2,
            }),
        }
    }

    #[test]
    fn client_messages_roundtrip() {
        for m in [
            hello(),
            ClientMessage::Input {
                seq: 9,
                move_x: -1,
                move_z: 1,
                yaw: 1.5,
            },
            ClientMessage::Interact {
                target: NetworkEntityId(42),
            },
            ClientMessage::RequestSnapshot,
            edit(),
        ] {
            let bytes = encode(&m).unwrap();
            assert_eq!(decode::<ClientMessage>(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn server_messages_roundtrip_including_world_op() {
        let ClientMessage::Edit { command, .. } = edit() else {
            unreachable!()
        };
        for m in [
            ServerMessage::Welcome {
                tick_rate_hz: 30,
                revision: 12,
                you: NetworkEntityId(3),
            },
            ServerMessage::SnapshotPart {
                index: 1,
                total: 3,
                data: vec![1, 2, 3],
            },
            ServerMessage::WorldOp(WorldOp { seq: 13, command }),
            ServerMessage::InputAck {
                seq: 9,
                pos: [1.0, 2.0, 3.0],
            },
            ServerMessage::EntityState {
                id: NetworkEntityId(4),
                pos: [0.0; 3],
                yaw: 1.0,
                tick: 77,
            },
            ServerMessage::EntityRemoved {
                id: NetworkEntityId(4),
            },
            ServerMessage::DoorState {
                net: NetworkEntityId(9),
                element: ObjectId::from_u128(5),
                open: true,
            },
            ServerMessage::Reject {
                reason: RejectReason::StaleRevision,
                detail: "revision".into(),
            },
        ] {
            let bytes = encode(&m).unwrap();
            assert_eq!(decode::<ServerMessage>(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn version_is_checked_before_body() {
        let mut bytes = encode(&hello()).unwrap();
        bytes[0] = 0xFF;
        assert!(matches!(
            decode::<ClientMessage>(&bytes),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn size_limits_apply_both_ways() {
        let big = vec![0u8; MAX_PACKET_BYTES + 1];
        assert_eq!(
            decode::<ClientMessage>(&big),
            Err(ProtocolError::TooLarge(MAX_PACKET_BYTES + 1))
        );
        assert_eq!(decode::<ClientMessage>(&[]), Err(ProtocolError::Truncated));
        assert_eq!(decode::<ClientMessage>(&[1]), Err(ProtocolError::Truncated));
        let m = ClientMessage::Hello {
            build: "x".repeat(MAX_BUILD_LEN + 1),
            world_id: "w".into(),
            world_hash: 0,
        };
        assert_eq!(encode(&m), Err(ProtocolError::Invalid("build")));
    }

    #[test]
    fn semantic_validation_rejects_bad_values() {
        let bad_axis = ClientMessage::Input {
            seq: 0,
            move_x: 5,
            move_z: 0,
            yaw: 0.0,
        };
        assert!(encode(&bad_axis).is_err());
        let nan = ClientMessage::Input {
            seq: 0,
            move_x: 0,
            move_z: 0,
            yaw: f32::NAN,
        };
        assert!(encode(&nan).is_err());

        // Те же значения, но «пришедшие по сети» в обход encode().
        let mut raw = PROTOCOL_VERSION.to_le_bytes().to_vec();
        raw.extend(postcard::to_stdvec(&bad_axis).unwrap());
        assert_eq!(
            decode::<ClientMessage>(&raw),
            Err(ProtocolError::Invalid("move axis"))
        );
        let mut raw = PROTOCOL_VERSION.to_le_bytes().to_vec();
        raw.extend(postcard::to_stdvec(&nan).unwrap());
        assert_eq!(
            decode::<ClientMessage>(&raw),
            Err(ProtocolError::Invalid("yaw"))
        );
    }

    #[test]
    fn terrain_messages_roundtrip_and_respect_limits() {
        for m in [
            ServerMessage::TerrainSnapshotPart {
                index: 0,
                total: 2,
                data: vec![1, 2, 3],
            },
            ServerMessage::TerrainOp {
                seq: 5,
                data: vec![9; 100],
            },
        ] {
            assert_eq!(decode::<ServerMessage>(&encode(&m).unwrap()).unwrap(), m);
        }
        let c = ClientMessage::EditTerrain {
            base_revision: 3,
            data: vec![7; 50],
        };
        assert_eq!(decode::<ClientMessage>(&encode(&c).unwrap()).unwrap(), c);
        assert!(
            encode(&ClientMessage::EditTerrain {
                base_revision: 0,
                data: vec![0; MAX_TERRAIN_EDIT_BYTES + 1]
            })
            .is_err()
        );
        assert!(
            encode(&ServerMessage::TerrainOp {
                seq: 0,
                data: vec![0; MAX_TERRAIN_EDIT_BYTES + 1]
            })
            .is_err()
        );
        assert!(
            encode(&ServerMessage::TerrainSnapshotPart {
                index: 3,
                total: 2,
                data: vec![]
            })
            .is_err()
        );
        assert!(
            encode(&ServerMessage::TerrainSnapshotPart {
                index: 0,
                total: 1,
                data: vec![0; MAX_SNAPSHOT_PART_BYTES + 1]
            })
            .is_err()
        );
    }

    #[test]
    fn server_message_validation() {
        let part = |index, total, len| ServerMessage::SnapshotPart {
            index,
            total,
            data: vec![0; len],
        };
        assert!(encode(&part(0, 1, 10)).is_ok());
        assert!(encode(&part(1, 1, 10)).is_err(), "индекс вне диапазона");
        assert!(encode(&part(0, 0, 10)).is_err());
        assert!(encode(&part(0, MAX_SNAPSHOT_PARTS + 1, 10)).is_err());
        assert!(encode(&part(0, 1, MAX_SNAPSHOT_PART_BYTES + 1)).is_err());
        assert!(
            encode(&ServerMessage::InputAck {
                seq: 1,
                pos: [f32::NAN, 0.0, 0.0]
            })
            .is_err()
        );
        assert!(
            encode(&ServerMessage::EntityState {
                id: NetworkEntityId(1),
                pos: [0.0; 3],
                yaw: f32::INFINITY,
                tick: 0
            })
            .is_err()
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&ClientMessage::RequestSnapshot).unwrap();
        bytes.push(0);
        assert_eq!(
            decode::<ClientMessage>(&bytes),
            Err(ProtocolError::TrailingBytes(1))
        );
    }

    #[test]
    fn invalid_world_command_data_is_rejected_at_decode() {
        // Некорректный PrototypeId не должен пройти десериализацию.
        let mut bytes = encode(&edit()).unwrap();
        let pos = bytes
            .windows(3)
            .position(|w| w == b"t:m")
            .expect("строка материала в пакете");
        bytes[pos] = b'T';
        assert!(decode::<ClientMessage>(&bytes).is_err());
    }

    /// Простейший детерминированный генератор — без внешних зависимостей.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut rng = XorShift(0x1234_5678_9ABC_DEF1);
        for _ in 0..20_000 {
            let len = (rng.next() % 200) as usize;
            let mut packet: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if packet.len() >= 2 && rng.next().is_multiple_of(2) {
                packet[..2].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
            }
            let _ = decode::<ClientMessage>(&packet);
            let _ = decode::<ServerMessage>(&packet);
        }
    }

    #[test]
    fn mutated_valid_packets_never_panic() {
        let mut rng = XorShift(0xDEAD_BEEF_CAFE_F00D);
        let seeds = [encode(&hello()).unwrap(), encode(&edit()).unwrap()];
        for _ in 0..20_000 {
            let mut p = seeds[(rng.next() % 2) as usize].clone();
            for _ in 0..(1 + rng.next() % 4) {
                let i = (rng.next() as usize) % p.len();
                p[i] = rng.next() as u8;
            }
            if rng.next().is_multiple_of(3) {
                p.truncate((rng.next() as usize) % p.len());
            }
            let _ = decode::<ClientMessage>(&p);
        }
    }
}
