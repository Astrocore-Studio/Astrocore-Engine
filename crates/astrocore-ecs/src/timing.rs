//! Игровое время: тики симуляции. Образец — `Robust.Shared/Timing/GameTiming.cs` @ 1afd692.

use std::time::Duration;

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

/// Номер тика симуляции. Тик 0 — «никогда»; первый тик раунда — 1.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct GameTick(pub u32);

impl GameTick {
    pub const ZERO: GameTick = GameTick(0);
    pub const FIRST: GameTick = GameTick(1);

    #[must_use]
    pub const fn next(self) -> GameTick {
        GameTick(self.0 + 1)
    }
}

/// Частота тиков по умолчанию, как у SS14 (`net.tickrate`).
pub const DEFAULT_TICK_RATE: u16 = 30;

/// Часы симуляции. Ресурс `World`.
#[derive(Resource, Debug, Clone)]
pub struct GameTime {
    /// Текущий тик.
    pub tick: GameTick,
    /// Тиков в секунду.
    pub tick_rate: u16,
    /// Симуляция остановлена целиком (не путать с паузой карты).
    pub paused: bool,
}

impl Default for GameTime {
    fn default() -> Self {
        Self {
            tick: GameTick::FIRST,
            tick_rate: DEFAULT_TICK_RATE,
            paused: false,
        }
    }
}

impl GameTime {
    /// Длительность одного тика.
    pub fn tick_period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.tick_rate.max(1)))
    }

    /// Длительность одного тика в секундах — `frameTime` систем Robust.
    pub fn frame_time(&self) -> f32 {
        1.0 / f32::from(self.tick_rate.max(1))
    }

    /// Время симуляции на начало текущего тика.
    pub fn cur_time(&self) -> Duration {
        self.time_of(self.tick)
    }

    /// Время симуляции на начало тика `tick`.
    pub fn time_of(&self, tick: GameTick) -> Duration {
        Duration::from_secs_f64(f64::from(tick.0) / f64::from(self.tick_rate.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_follows_ticks() {
        let mut time = GameTime::default();
        assert_eq!(time.tick_period(), Duration::from_secs_f64(1.0 / 30.0));
        time.tick = GameTick(90);
        assert_eq!(time.cur_time(), Duration::from_secs(3));
    }
}
