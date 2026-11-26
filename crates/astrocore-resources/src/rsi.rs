//! Описание набора спрайтов RSI (`meta.json`). Формат — как в Robust
//! (`Robust.Shared/Resources/RsiLoading.cs` @ 1afd692): каталог `.rsi` хранит по картинке на
//! состояние; картинка — сетка кадров, строки сетки заполняются слева направо, сначала все
//! кадры первого направления, затем второго.

use serde::Deserialize;

use crate::ResourceError;

#[derive(Debug, thiserror::Error)]
pub enum RsiError {
    #[error(transparent)]
    Resource(#[from] ResourceError),
    #[error("meta.json: {0}")]
    Parse(String),
}

/// Число направлений состояния.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RsiDirections {
    /// Один вид со всех сторон.
    #[default]
    One,
    /// Юг, север, восток, запад.
    Four,
    /// Восемь направлений.
    Eight,
}

impl RsiDirections {
    pub const fn count(self) -> usize {
        match self {
            RsiDirections::One => 1,
            RsiDirections::Four => 4,
            RsiDirections::Eight => 8,
        }
    }
}

/// Состояние набора: имя файла картинки, направления и длительности кадров.
#[derive(Debug, Clone, PartialEq)]
pub struct RsiState {
    pub name: String,
    pub directions: RsiDirections,
    /// Длительности кадров в секундах: по списку на направление. Пусто — один кадр.
    pub delays: Vec<Vec<f32>>,
}

impl RsiState {
    /// Число кадров в одном направлении.
    pub fn frames(&self) -> usize {
        self.delays.first().map_or(1, |delays| delays.len().max(1))
    }

    /// Кадр анимации направления в момент `time` секунд от её начала (анимация зациклена).
    pub fn frame_at(&self, direction: usize, time: f32) -> usize {
        let Some(delays) = self.delays.get(direction).or(self.delays.first()) else {
            return 0;
        };
        let total: f32 = delays.iter().sum();
        if total <= 0.0 || delays.len() < 2 {
            return 0;
        }
        let mut left = time.rem_euclid(total);
        for (frame, delay) in delays.iter().enumerate() {
            if left < *delay {
                return frame;
            }
            left -= delay;
        }
        delays.len() - 1
    }

    /// Номер кадра в сетке картинки: сначала кадры первого направления, затем следующего.
    pub fn sheet_index(&self, direction: usize, frame: usize) -> usize {
        direction.min(self.directions.count() - 1) * self.frames() + frame.min(self.frames() - 1)
    }
}

/// Описание набора спрайтов.
#[derive(Debug, Clone, PartialEq)]
pub struct RsiMeta {
    /// Размер кадра в пикселях.
    pub size: (u32, u32),
    pub license: Option<String>,
    pub copyright: Option<String>,
    pub states: Vec<RsiState>,
}

#[derive(Deserialize)]
struct RawMeta {
    size: RawSize,
    license: Option<String>,
    copyright: Option<String>,
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
    #[serde(default)]
    directions: Option<u32>,
    #[serde(default)]
    delays: Option<Vec<Vec<f32>>>,
}

impl RsiMeta {
    pub fn parse(json: &str) -> Result<Self, RsiError> {
        let raw: RawMeta = serde_json::from_str(json.trim_start_matches('\u{feff}'))
            .map_err(|e| RsiError::Parse(e.to_string()))?;
        if raw.size.x == 0 || raw.size.y == 0 {
            return Err(RsiError::Parse("нулевой размер кадра".into()));
        }
        let mut states = Vec::with_capacity(raw.states.len());
        for state in raw.states {
            let directions = match state.directions.unwrap_or(1) {
                1 => RsiDirections::One,
                4 => RsiDirections::Four,
                8 => RsiDirections::Eight,
                other => {
                    return Err(RsiError::Parse(format!(
                        "состояние {}: направлений может быть 1, 4 или 8, задано {other}",
                        state.name
                    )));
                }
            };
            let delays = state.delays.unwrap_or_default();
            if !delays.is_empty() && delays.len() != directions.count() {
                return Err(RsiError::Parse(format!(
                    "состояние {}: списков длительностей {}, направлений {}",
                    state.name,
                    delays.len(),
                    directions.count()
                )));
            }
            states.push(RsiState {
                name: state.name,
                directions,
                delays,
            });
        }
        Ok(Self {
            size: (raw.size.x, raw.size.y),
            license: raw.license,
            copyright: raw.copyright,
            states,
        })
    }

    pub fn state(&self, name: &str) -> Option<&RsiState> {
        self.states.iter().find(|state| state.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"{
        "version": 1, "license": "CC-BY-SA-3.0", "copyright": "tgstation",
        "size": {"x": 32, "y": 32},
        "states": [
            {"name": "icon"},
            {"name": "closed", "directions": 4},
            {"name": "opening", "directions": 4,
             "delays": [[0.1, 0.2, 0.3], [0.1, 0.2, 0.3], [0.1, 0.2, 0.3], [0.1, 0.2, 0.3]]}
        ]
    }"#;

    #[test]
    fn parses_states_directions_and_delays() {
        let meta = RsiMeta::parse(META).unwrap();
        assert_eq!(meta.size, (32, 32));
        assert_eq!(meta.license.as_deref(), Some("CC-BY-SA-3.0"));
        assert_eq!(meta.state("icon").unwrap().frames(), 1);
        assert_eq!(
            meta.state("closed").unwrap().directions,
            RsiDirections::Four
        );
        let opening = meta.state("opening").unwrap();
        assert_eq!(opening.frames(), 3);
        assert_eq!(opening.frame_at(0, 0.05), 0);
        assert_eq!(opening.frame_at(0, 0.25), 1);
        assert_eq!(opening.frame_at(0, 0.59), 2);
        assert_eq!(opening.frame_at(0, 0.65), 0);
        assert_eq!(opening.sheet_index(2, 1), 7);
        assert_eq!(opening.sheet_index(9, 9), 11);
        assert!(meta.state("missing").is_none());
    }

    #[test]
    fn rejects_inconsistent_meta() {
        assert!(RsiMeta::parse(r#"{"size": {"x": 0, "y": 32}}"#).is_err());
        assert!(
            RsiMeta::parse(
                r#"{"size": {"x": 32, "y": 32}, "states": [{"name": "a", "directions": 3}]}"#
            )
            .is_err()
        );
        assert!(
            RsiMeta::parse(r#"{"size": {"x": 32, "y": 32}, "states": [{"name": "a", "directions": 4, "delays": [[1.0]]}]}"#)
                .is_err()
        );
        assert!(RsiMeta::parse("не json").is_err());
    }
}
