use std::collections::{BTreeMap, BTreeSet};

use astrocore_core::Revision;
use astrocore_math::ChunkCoord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkState {
    Unloaded,
    /// Известны границы и содержимое индекса, данные симуляции не загружены.
    MetadataOnly,
    /// Идёт фоновая сборка для ревизии `for_revision`.
    Loading {
        for_revision: Revision,
    },
    Resident,
    Unloading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAction {
    LoadMetadata(ChunkCoord),
    StartLoad(ChunkCoord),
    StartUnload(ChunkCoord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOutcome {
    /// Результат принят, чанк стал `Resident`.
    Accepted,
    /// Чанк изменился за время загрузки — результат отброшен, нужна новая загрузка.
    Stale,
    /// Чанк не ожидал результата (отменён или выгружен).
    Unexpected,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamConfig {
    /// Чанки в этом радиусе (Чебышёв) вокруг фокуса загружаются полностью.
    pub load_radius: i32,
    /// Чанки в этом радиусе держат метаданные.
    pub metadata_radius: i32,
    /// Дополнительный гистерезис выгрузки: уходим на `radius + hysteresis`.
    pub hysteresis: i32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            load_radius: 2,
            metadata_radius: 4,
            hysteresis: 1,
        }
    }
}

#[derive(Debug, Default)]
pub struct Streamer {
    config: StreamConfig,
    states: BTreeMap<ChunkCoord, ChunkState>,
}

pub fn chebyshev(a: ChunkCoord, b: ChunkCoord) -> i32 {
    (a.x - b.x).abs().max((a.z - b.z).abs())
}

impl Streamer {
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config,
            states: BTreeMap::new(),
        }
    }

    pub fn state(&self, chunk: ChunkCoord) -> ChunkState {
        self.states
            .get(&chunk)
            .copied()
            .unwrap_or(ChunkState::Unloaded)
    }

    pub fn resident(&self) -> impl Iterator<Item = ChunkCoord> + '_ {
        self.states
            .iter()
            .filter(|(_, s)| **s == ChunkState::Resident)
            .map(|(c, _)| *c)
    }

    fn dist_to_focus(chunk: ChunkCoord, focus: &[ChunkCoord]) -> i32 {
        focus
            .iter()
            .map(|f| chebyshev(chunk, *f))
            .min()
            .unwrap_or(i32::MAX)
    }

    /// Рассчитать действия для текущих фокусов (позиции игроков). Состояния
    /// обновляются сразу (`StartLoad` → `Loading`, `StartUnload` → `Unloading`).
    /// `revision_of` возвращает текущую ревизию чанка в мире.
    /// Порядок действий детерминирован.
    pub fn plan(
        &mut self,
        focus: &[ChunkCoord],
        revision_of: impl Fn(ChunkCoord) -> Revision,
    ) -> Vec<StreamAction> {
        let mut actions = Vec::new();
        let cfg = self.config;
        let mut wanted: BTreeSet<ChunkCoord> = BTreeSet::new();
        for f in focus {
            for dx in -cfg.metadata_radius..=cfg.metadata_radius {
                for dz in -cfg.metadata_radius..=cfg.metadata_radius {
                    wanted.insert(ChunkCoord::new(f.x + dx, f.z + dz));
                }
            }
        }
        for chunk in wanted {
            let d = Self::dist_to_focus(chunk, focus);
            match self.state(chunk) {
                ChunkState::Unloaded => {
                    self.states.insert(chunk, ChunkState::MetadataOnly);
                    actions.push(StreamAction::LoadMetadata(chunk));
                    if d <= cfg.load_radius {
                        self.states.insert(
                            chunk,
                            ChunkState::Loading {
                                for_revision: revision_of(chunk),
                            },
                        );
                        actions.push(StreamAction::StartLoad(chunk));
                    }
                }
                ChunkState::MetadataOnly if d <= cfg.load_radius => {
                    self.states.insert(
                        chunk,
                        ChunkState::Loading {
                            for_revision: revision_of(chunk),
                        },
                    );
                    actions.push(StreamAction::StartLoad(chunk));
                }
                _ => {}
            }
        }
        // Выгрузка: всё, что ушло дальше радиуса + гистерезис.
        let to_unload: Vec<ChunkCoord> = self
            .states
            .iter()
            .filter(|(c, s)| {
                let d = Self::dist_to_focus(**c, focus);
                matches!(s, ChunkState::Resident | ChunkState::Loading { .. })
                    && d > cfg.load_radius + cfg.hysteresis
            })
            .map(|(c, _)| *c)
            .collect();
        for chunk in to_unload {
            self.states.insert(chunk, ChunkState::Unloading);
            actions.push(StreamAction::StartUnload(chunk));
        }
        actions
    }

    /// Фоновая загрузка завершилась. `current` — ревизия чанка в мире сейчас.
    pub fn complete_load(
        &mut self,
        chunk: ChunkCoord,
        built_for: Revision,
        current: Revision,
    ) -> LoadOutcome {
        match self.state(chunk) {
            ChunkState::Loading { .. } if built_for == current => {
                self.states.insert(chunk, ChunkState::Resident);
                LoadOutcome::Accepted
            }
            ChunkState::Loading { .. } => {
                // Результат устарел: запускаем загрузку заново для актуальной ревизии.
                self.states.insert(
                    chunk,
                    ChunkState::Loading {
                        for_revision: current,
                    },
                );
                LoadOutcome::Stale
            }
            _ => LoadOutcome::Unexpected,
        }
    }

    /// Выгрузка завершена: данные сохранены, остаются метаданные.
    pub fn complete_unload(&mut self, chunk: ChunkCoord) -> bool {
        if self.state(chunk) == ChunkState::Unloading {
            self.states.insert(chunk, ChunkState::MetadataOnly);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Activity {
    Dormant,
    Reduced,
    Active,
}

/// Что система умеет делать при пониженной активности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducedBehavior {
    /// Считать как обычно, но реже.
    SlowTick,
    /// Перейти на агрегированную модель (доказанно корректную для системы).
    Aggregate,
    /// Корректной упрощённой модели нет: всегда считать полностью.
    NotAllowed,
}

#[derive(Debug, Clone, Copy)]
pub struct ActivityConfig {
    pub active_radius: i32,
    pub reduced_radius: i32,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            active_radius: 1,
            reduced_radius: 3,
        }
    }
}

#[derive(Debug, Default)]
pub struct ActivityPlanner {
    config: ActivityConfig,
    pinned: BTreeSet<ChunkCoord>,
}

impl ActivityPlanner {
    pub fn new(config: ActivityConfig) -> Self {
        Self {
            config,
            pinned: BTreeSet::new(),
        }
    }

    /// Удерживать чанк: в нём идёт процесс, который нельзя заморозить.
    pub fn pin(&mut self, chunk: ChunkCoord) {
        self.pinned.insert(chunk);
    }

    pub fn unpin(&mut self, chunk: ChunkCoord) {
        self.pinned.remove(&chunk);
    }

    /// Базовый уровень по расстоянию до игроков.
    pub fn base_activity(&self, chunk: ChunkCoord, focus: &[ChunkCoord]) -> Activity {
        let d = Streamer::dist_to_focus(chunk, focus);
        if d <= self.config.active_radius {
            Activity::Active
        } else if d <= self.config.reduced_radius {
            Activity::Reduced
        } else {
            Activity::Dormant
        }
    }

    /// Итоговый уровень для конкретной системы.
    ///
    /// * `NotAllowed` — всегда `Active`;
    /// * закреплённый чанк (и его соседи — процессы перетекают через границы) не может
    ///   быть `Dormant`: минимум `Reduced`.
    pub fn effective(
        &self,
        chunk: ChunkCoord,
        focus: &[ChunkCoord],
        behavior: ReducedBehavior,
    ) -> Activity {
        if behavior == ReducedBehavior::NotAllowed {
            return Activity::Active;
        }
        let base = self.base_activity(chunk, focus);
        let near_pinned = self.pinned.iter().any(|p| chebyshev(*p, chunk) <= 1);
        if near_pinned && base == Activity::Dormant {
            Activity::Reduced
        } else {
            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R0: Revision = Revision(0);

    fn c(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(x, z)
    }

    #[test]
    fn plan_loads_nearby_and_keeps_metadata_farther() {
        let mut s = Streamer::new(StreamConfig {
            load_radius: 1,
            metadata_radius: 2,
            hysteresis: 1,
        });
        let actions = s.plan(&[c(0, 0)], |_| R0);
        let loads = actions
            .iter()
            .filter(|a| matches!(a, StreamAction::StartLoad(_)))
            .count();
        let meta = actions
            .iter()
            .filter(|a| matches!(a, StreamAction::LoadMetadata(_)))
            .count();
        assert_eq!((loads, meta), (9, 25));
        assert!(matches!(s.state(c(0, 0)), ChunkState::Loading { .. }));
        assert_eq!(s.state(c(2, 2)), ChunkState::MetadataOnly);
        assert_eq!(s.state(c(3, 3)), ChunkState::Unloaded);
        assert!(
            s.plan(&[c(0, 0)], |_| R0).is_empty(),
            "повторный расчёт без изменений ничего не делает"
        );
    }

    #[test]
    fn plan_is_deterministic() {
        let run = || Streamer::new(StreamConfig::default()).plan(&[c(5, -3), c(-1, 2)], |_| R0);
        assert_eq!(run(), run());
    }

    #[test]
    fn stale_load_results_are_rejected_and_restarted() {
        let mut s = Streamer::new(StreamConfig::default());
        s.plan(&[c(0, 0)], |_| Revision(7));
        // За время загрузки чанк изменили: ревизия стала 9.
        assert_eq!(
            s.complete_load(c(0, 0), Revision(7), Revision(9)),
            LoadOutcome::Stale
        );
        assert_eq!(
            s.state(c(0, 0)),
            ChunkState::Loading {
                for_revision: Revision(9)
            }
        );
        assert_eq!(
            s.complete_load(c(0, 0), Revision(9), Revision(9)),
            LoadOutcome::Accepted
        );
        assert_eq!(s.state(c(0, 0)), ChunkState::Resident);
        assert_eq!(
            s.complete_load(c(0, 0), Revision(9), Revision(9)),
            LoadOutcome::Unexpected
        );
        assert_eq!(s.complete_load(c(50, 50), R0, R0), LoadOutcome::Unexpected);
    }

    #[test]
    fn unload_uses_hysteresis_and_returns_to_metadata() {
        let cfg = StreamConfig {
            load_radius: 1,
            metadata_radius: 3,
            hysteresis: 1,
        };
        let mut s = Streamer::new(cfg);
        s.plan(&[c(0, 0)], |_| R0);
        s.complete_load(c(1, 0), R0, R0);
        // Игрок сдвинулся на один чанк: (1,0) всё ещё в зоне гистерезиса — не выгружается.
        let a = s.plan(&[c(0, 2)], |_| R0);
        assert!(
            !a.contains(&StreamAction::StartUnload(c(1, 0))) || chebyshev(c(1, 0), c(0, 2)) > 2
        );
        // Далеко — выгружается.
        let a = s.plan(&[c(0, 10)], |_| R0);
        assert!(a.contains(&StreamAction::StartUnload(c(1, 0))));
        assert!(s.complete_unload(c(1, 0)));
        assert_eq!(s.state(c(1, 0)), ChunkState::MetadataOnly);
        assert!(!s.complete_unload(c(1, 0)), "дважды нельзя");
    }

    #[test]
    fn resident_chunks_are_listed() {
        let mut s = Streamer::new(StreamConfig {
            load_radius: 0,
            metadata_radius: 1,
            hysteresis: 0,
        });
        s.plan(&[c(0, 0)], |_| R0);
        assert_eq!(s.resident().count(), 0);
        s.complete_load(c(0, 0), R0, R0);
        assert_eq!(s.resident().collect::<Vec<_>>(), vec![c(0, 0)]);
    }

    #[test]
    fn activity_follows_distance() {
        let p = ActivityPlanner::new(ActivityConfig {
            active_radius: 1,
            reduced_radius: 3,
        });
        let f = [c(0, 0)];
        assert_eq!(p.base_activity(c(1, 1), &f), Activity::Active);
        assert_eq!(p.base_activity(c(3, 0), &f), Activity::Reduced);
        assert_eq!(p.base_activity(c(4, 0), &f), Activity::Dormant);
        assert_eq!(
            p.base_activity(c(4, 0), &[]),
            Activity::Dormant,
            "без игроков всё спит"
        );
        assert!(Activity::Active > Activity::Reduced && Activity::Reduced > Activity::Dormant);
    }

    #[test]
    fn hazards_and_unreducible_systems_are_never_frozen() {
        let mut p = ActivityPlanner::new(ActivityConfig {
            active_radius: 1,
            reduced_radius: 3,
        });
        let f = [c(0, 0)];
        let far = c(10, 10);
        assert_eq!(
            p.effective(far, &f, ReducedBehavior::Aggregate),
            Activity::Dormant
        );
        p.pin(c(10, 10)); // пожар в чанке
        assert_eq!(
            p.effective(far, &f, ReducedBehavior::Aggregate),
            Activity::Reduced
        );
        assert_eq!(
            p.effective(c(11, 10), &f, ReducedBehavior::SlowTick),
            Activity::Reduced,
            "соседний тоже"
        );
        assert_eq!(
            p.effective(c(13, 10), &f, ReducedBehavior::SlowTick),
            Activity::Dormant
        );
        assert_eq!(
            p.effective(c(40, 40), &f, ReducedBehavior::NotAllowed),
            Activity::Active
        );
        p.unpin(c(10, 10));
        assert_eq!(
            p.effective(far, &f, ReducedBehavior::Aggregate),
            Activity::Dormant
        );
    }
}
