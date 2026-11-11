use std::collections::HashSet;

use serde::Deserialize;

use crate::DirectionSet;

pub const MAX_STATES: usize = 20_000;
pub const MAX_FRAMES_PER_STATE: usize = 512;
pub const MAX_SPRITE_SIZE: u32 = 4096;
pub const MAX_META_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RsiError {
    #[error("meta.json слишком велик ({0} байт)")]
    TooLarge(usize),
    #[error("некорректный JSON: {0}")]
    Json(String),
    #[error("неподдерживаемая версия RSI: {0}")]
    UnsupportedVersion(u32),
    #[error("недопустимый размер спрайта {x}×{y}")]
    BadSize { x: u32, y: u32 },
    #[error("слишком много состояний ({0})")]
    TooManyStates(usize),
    #[error("состояние {0:?} повторяется")]
    DuplicateState(String),
    #[error("состояние {state:?}: неверное число направлений {directions}")]
    BadDirections { state: String, directions: u8 },
    #[error("состояние {state:?}: {reason}")]
    BadDelays { state: String, reason: String },
    #[error("имя состояния {0:?} недопустимо")]
    BadStateName(String),
}

#[derive(Deserialize)]
struct RawMeta {
    version: u32,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
    size: RawSize,
    #[serde(default)]
    states: Vec<RawState>,
}

#[derive(Deserialize)]
struct RawSize {
    x: u32,
    y: u32,
}

#[derive(Deserialize)]
struct RawState {
    name: String,
    #[serde(default = "one")]
    directions: u8,
    #[serde(default)]
    delays: Option<Vec<Vec<f32>>>,
}

fn one() -> u8 {
    1
}

/// Анимационное состояние спрайта.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteState {
    pub name: String,
    pub directions: DirectionSet,
    /// Задержки кадров в секундах для каждого направления. Для неанимированного
    /// состояния — один кадр без задержки (`vec![vec![]]` не используется).
    pub delays: Vec<Vec<f32>>,
}

impl SpriteState {
    pub fn frame_count(&self) -> usize {
        self.delays.first().map_or(1, Vec::len).max(1)
    }

    pub fn is_animated(&self) -> bool {
        self.frame_count() > 1
    }

    /// Полная длительность цикла анимации в секундах (для первого направления).
    pub fn cycle_seconds(&self) -> f32 {
        self.delays.first().map_or(0.0, |d| d.iter().sum())
    }
}

/// Метаданные RSI-набора.
#[derive(Debug, Clone, PartialEq)]
pub struct RsiMeta {
    pub size: (u32, u32),
    pub license: Option<String>,
    pub copyright: Option<String>,
    pub states: Vec<SpriteState>,
}

impl RsiMeta {
    pub fn parse(bytes: &[u8]) -> Result<RsiMeta, RsiError> {
        if bytes.len() > MAX_META_BYTES {
            return Err(RsiError::TooLarge(bytes.len()));
        }
        let raw: RawMeta =
            serde_json::from_slice(bytes).map_err(|e| RsiError::Json(e.to_string()))?;
        if raw.version != 1 {
            return Err(RsiError::UnsupportedVersion(raw.version));
        }
        let (x, y) = (raw.size.x, raw.size.y);
        if x == 0 || y == 0 || x > MAX_SPRITE_SIZE || y > MAX_SPRITE_SIZE {
            return Err(RsiError::BadSize { x, y });
        }
        if raw.states.len() > MAX_STATES {
            return Err(RsiError::TooManyStates(raw.states.len()));
        }
        let mut names = HashSet::new();
        let mut states = Vec::with_capacity(raw.states.len());
        for s in raw.states {
            if s.name.is_empty() || s.name.len() > 256 || s.name.chars().any(char::is_control) {
                return Err(RsiError::BadStateName(s.name));
            }
            if !names.insert(s.name.clone()) {
                return Err(RsiError::DuplicateState(s.name));
            }
            let directions =
                DirectionSet::from_count(s.directions).ok_or(RsiError::BadDirections {
                    state: s.name.clone(),
                    directions: s.directions,
                })?;
            let dir_count = usize::from(s.directions);
            let delays = match s.delays {
                None => vec![Vec::new(); dir_count],
                Some(d) => d,
            };
            if delays.len() != dir_count {
                return Err(RsiError::BadDelays {
                    state: s.name,
                    reason: format!(
                        "ожидалось {dir_count} списков задержек, получено {}",
                        delays.len()
                    ),
                });
            }
            let frames = delays.first().map_or(0, Vec::len);
            if frames > MAX_FRAMES_PER_STATE {
                return Err(RsiError::BadDelays {
                    state: s.name,
                    reason: format!("слишком много кадров ({frames})"),
                });
            }
            if delays.iter().any(|d| d.len() != frames) {
                return Err(RsiError::BadDelays {
                    state: s.name,
                    reason: "число кадров различается между направлениями".into(),
                });
            }
            if delays.iter().flatten().any(|d| !d.is_finite() || *d <= 0.0) {
                return Err(RsiError::BadDelays {
                    state: s.name,
                    reason: "задержки должны быть конечными и положительными".into(),
                });
            }
            states.push(SpriteState {
                name: s.name,
                directions,
                delays,
            });
        }
        Ok(RsiMeta {
            size: (x, y),
            license: raw.license,
            copyright: raw.copyright,
            states,
        })
    }

    pub fn state(&self, name: &str) -> Option<&SpriteState> {
        self.states.iter().find(|s| s.name == name)
    }

    /// Политика лицензий (раздел 17): набор без явной лицензии блокирует экспорт
    /// публичного ассетного пакета.
    pub fn has_license(&self) -> bool {
        self.license
            .as_deref()
            .is_some_and(|l| !l.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
        "version": 1, "license": "CC-BY-SA-3.0", "copyright": "Authors",
        "size": {"x": 32, "y": 32},
        "states": [
            {"name": "idle", "directions": 4},
            {"name": "walk", "directions": 4, "delays": [[0.1,0.1],[0.1,0.1],[0.1,0.1],[0.1,0.1]]},
            {"name": "item"}
        ]
    }"#;

    #[test]
    fn parses_valid_meta() {
        let m = RsiMeta::parse(GOOD.as_bytes()).unwrap();
        assert_eq!(m.size, (32, 32));
        assert!(m.has_license());
        assert_eq!(m.states.len(), 3);
        let walk = m.state("walk").unwrap();
        assert!(walk.is_animated());
        assert_eq!(walk.frame_count(), 2);
        assert!((walk.cycle_seconds() - 0.2).abs() < 1e-6);
        assert_eq!(walk.directions, DirectionSet::Four);
        let idle = m.state("idle").unwrap();
        assert!(!idle.is_animated());
        assert_eq!(m.state("item").unwrap().directions, DirectionSet::One);
        assert!(m.state("nope").is_none());
    }

    #[test]
    fn missing_license_is_detected() {
        let m = RsiMeta::parse(br#"{"version":1,"size":{"x":32,"y":32},"states":[]}"#).unwrap();
        assert!(!m.has_license());
        let blank =
            RsiMeta::parse(br#"{"version":1,"license":"  ","size":{"x":32,"y":32}}"#).unwrap();
        assert!(!blank.has_license());
    }

    #[test]
    fn rejects_malformed_and_hostile_input() {
        let bad = |s: &str| RsiMeta::parse(s.as_bytes());
        assert!(matches!(bad("not json"), Err(RsiError::Json(_))));
        assert!(matches!(
            bad(r#"{"version":2,"size":{"x":1,"y":1}}"#),
            Err(RsiError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            bad(r#"{"version":1,"size":{"x":0,"y":32}}"#),
            Err(RsiError::BadSize { .. })
        ));
        assert!(matches!(
            bad(r#"{"version":1,"size":{"x":99999,"y":32}}"#),
            Err(RsiError::BadSize { .. })
        ));
        assert!(matches!(
            bad(r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":"a"},{"name":"a"}]}"#),
            Err(RsiError::DuplicateState(_))
        ));
        assert!(matches!(
            bad(r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":"a","directions":3}]}"#),
            Err(RsiError::BadDirections { .. })
        ));
        assert!(matches!(
            bad(
                r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":"a","directions":4,"delays":[[0.1]]}]}"#
            ),
            Err(RsiError::BadDelays { .. })
        ));
        assert!(matches!(
            bad(
                r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":"a","directions":1,"delays":[[-1.0]]}]}"#
            ),
            Err(RsiError::BadDelays { .. })
        ));
        assert!(matches!(
            bad(
                r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":"a","directions":4,"delays":[[0.1],[0.1,0.1],[0.1],[0.1]]}]}"#
            ),
            Err(RsiError::BadDelays { .. })
        ));
        assert!(matches!(
            bad(r#"{"version":1,"size":{"x":32,"y":32},"states":[{"name":""}]}"#),
            Err(RsiError::BadStateName(_))
        ));
        let huge = vec![b' '; MAX_META_BYTES + 1];
        assert!(matches!(RsiMeta::parse(&huge), Err(RsiError::TooLarge(_))));
    }

    #[test]
    fn many_frames_are_limited() {
        let frames = vec!["0.1"; MAX_FRAMES_PER_STATE + 1].join(",");
        let json = format!(
            r#"{{"version":1,"size":{{"x":32,"y":32}},"states":[{{"name":"a","directions":1,"delays":[[{frames}]]}}]}}"#
        );
        assert!(matches!(
            RsiMeta::parse(json.as_bytes()),
            Err(RsiError::BadDelays { .. })
        ));
    }
}
