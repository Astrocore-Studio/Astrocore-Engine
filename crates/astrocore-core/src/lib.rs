mod entities;

pub use entities::{EntityError, EntityRegistry, RuntimeEntityId};

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Постоянный идентификатор размещённого объекта (UUID). Не меняется при
/// сохранении/загрузке, перемещении объекта и перезапуске сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(Uuid);

impl ObjectId {
    /// Новый случайный идентификатор.
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Идентификатор из числа — для тестов и фикстур, где нужна воспроизводимость.
    pub const fn from_u128(v: u128) -> Self {
        Self(Uuid::from_u128(v))
    }

    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub const fn from_uuid(v: Uuid) -> Self {
        Self(v)
    }

    /// Детерминированный идентификатор, выведенный из пространства имён и «имени»
    /// (UUID v5). Нужен для объектов, порождаемых префабами: тот же узел того же
    /// экземпляра всегда получает тот же `ObjectId`.
    pub fn derive(namespace: &ObjectId, name: &[u8]) -> Self {
        Self(Uuid::new_v5(&namespace.0, name))
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Стабильный идентификатор узла внутри определения префаба.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrefabNodeId(Uuid);

impl PrefabNodeId {
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub const fn from_u128(v: u128) -> Self {
        Self(Uuid::from_u128(v))
    }
}

/// Идентификатор экземпляра префаба в мире.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrefabInstanceId(Uuid);

impl PrefabInstanceId {
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub const fn from_u128(v: u128) -> Self {
        Self(Uuid::from_u128(v))
    }
}

/// Путь вложенного объекта: экземпляр префаба + узел внутри него.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NestedObjectRef {
    pub instance: PrefabInstanceId,
    pub node: PrefabNodeId,
}

/// Идентификатор сущности в рамках одной сетевой сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkEntityId(pub u32);

/// Монотонная ревизия мира или области.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const ZERO: Revision = Revision(0);

    #[must_use]
    pub const fn next(self) -> Revision {
        Revision(self.0 + 1)
    }
}

/// Номер версии схемы сериализуемых данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("идентификатор прототипа пуст")]
    Empty,
    #[error("идентификатор прототипа {0:?} длиннее {PROTOTYPE_ID_MAX_LEN} символов")]
    TooLong(String),
    #[error("идентификатор прототипа {0:?} должен иметь вид `пространство:имя`")]
    MissingNamespace(String),
    #[error("идентификатор прототипа {0:?} содержит недопустимый символ {1:?}")]
    BadChar(String, char),
}

pub const PROTOTYPE_ID_MAX_LEN: usize = 128;

/// Стабильный строковый идентификатор типа ресурса: `namespace:path/name`.
///
/// Допустимы `a-z`, `0-9`, `_`, `-`, `.`, `/` и ровно один `:`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PrototypeId(String);

impl PrototypeId {
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(IdError::Empty);
        }
        if s.len() > PROTOTYPE_ID_MAX_LEN {
            return Err(IdError::TooLong(s));
        }
        let mut colons = 0;
        for c in s.chars() {
            match c {
                'a'..='z' | '0'..='9' | '_' | '-' | '.' | '/' => {}
                ':' => colons += 1,
                other => return Err(IdError::BadChar(s, other)),
            }
        }
        let (ns, name) = s.split_once(':').unwrap_or(("", ""));
        if colons != 1 || ns.is_empty() || name.is_empty() {
            return Err(IdError::MissingNamespace(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn namespace(&self) -> &str {
        self.0.split_once(':').map_or("", |(ns, _)| ns)
    }
}

impl TryFrom<String> for PrototypeId {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PrototypeId> for String {
    fn from(v: PrototypeId) -> Self {
        v.0
    }
}

impl fmt::Display for PrototypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_id_accepts_valid() {
        let id = PrototypeId::new("astrocore:structure/wall_stone-01").unwrap();
        assert_eq!(id.namespace(), "astrocore");
        assert_eq!(id.to_string(), "astrocore:structure/wall_stone-01");
    }

    #[test]
    fn prototype_id_rejects_invalid() {
        assert_eq!(PrototypeId::new(""), Err(IdError::Empty));
        assert!(matches!(
            PrototypeId::new("wall"),
            Err(IdError::MissingNamespace(_))
        ));
        assert!(matches!(
            PrototypeId::new(":wall"),
            Err(IdError::MissingNamespace(_))
        ));
        assert!(matches!(
            PrototypeId::new("a:"),
            Err(IdError::MissingNamespace(_))
        ));
        assert!(matches!(
            PrototypeId::new("a:b:c"),
            Err(IdError::MissingNamespace(_))
        ));
        assert!(matches!(
            PrototypeId::new("A:b"),
            Err(IdError::BadChar(_, 'A'))
        ));
        assert!(matches!(
            PrototypeId::new("a:b c"),
            Err(IdError::BadChar(_, ' '))
        ));
        assert!(matches!(
            PrototypeId::new(format!("a:{}", "x".repeat(200))),
            Err(IdError::TooLong(_))
        ));
    }

    #[test]
    fn prototype_id_deserialization_validates() {
        let ok: PrototypeId = serde_json::from_str("\"a:b\"").unwrap();
        assert_eq!(ok.as_str(), "a:b");
        assert!(serde_json::from_str::<PrototypeId>("\"nope\"").is_err());
    }

    #[test]
    fn object_id_roundtrips_through_json() {
        let id = ObjectId::from_u128(0xDEADBEEF);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ObjectId>(&s).unwrap(), id);
    }

    #[test]
    fn derived_ids_are_deterministic_and_distinct() {
        let ns = ObjectId::from_u128(1);
        assert_eq!(ObjectId::derive(&ns, b"a"), ObjectId::derive(&ns, b"a"));
        assert_ne!(ObjectId::derive(&ns, b"a"), ObjectId::derive(&ns, b"b"));
        assert_ne!(
            ObjectId::derive(&ns, b"a"),
            ObjectId::derive(&ObjectId::from_u128(2), b"a")
        );
    }

    #[test]
    fn random_ids_are_unique() {
        assert_ne!(ObjectId::new_random(), ObjectId::new_random());
    }

    #[test]
    fn revision_is_monotonic() {
        assert!(Revision::ZERO.next() > Revision::ZERO);
        assert_eq!(Revision(41).next(), Revision(42));
    }
}
