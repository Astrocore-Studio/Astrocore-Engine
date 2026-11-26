pub mod inheritance;

use std::fmt;

use serde::{Deserialize, Serialize};

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
}
