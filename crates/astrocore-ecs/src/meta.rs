//! Метаданные сущности. Образец — `Robust.Shared/GameObjects/Components/MetaData` @ 1afd692.

use std::sync::Arc;
use std::time::Duration;

use bevy_ecs::component::Component;

use crate::timing::GameTick;

/// Стадия жизни сущности (`EntityLifeStage` Robust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LifeStage {
    /// Создана, компоненты ещё не инициализированы.
    #[default]
    Spawned,
    /// Идёт инициализация компонентов.
    Initializing,
    /// Компоненты инициализированы и запущены.
    Initialized,
    /// Прошла разовая настройка на инициализированной карте (`MapInit`).
    MapInitialized,
    /// Удаляется: `EntityTerminating` уже поднято.
    Terminating,
    /// Удалена.
    Deleted,
}

/// Метаданные: есть у каждой сущности, созданной через [`crate::LifecycleWorldExt`].
#[derive(Component, Debug, Clone, Default)]
pub struct MetaData {
    /// Имя; `None` — берётся из прототипа.
    pub name: Option<String>,
    /// Описание; `None` — берётся из прототипа.
    pub description: Option<String>,
    /// Прототип, из которого создана сущность.
    pub prototype: Option<Arc<str>>,
    pub stage: LifeStage,
    /// Компоненты запущены (`ComponentStartup` поднят).
    pub started: bool,
    /// Тик создания.
    pub created: GameTick,
    /// Время симуляции, когда сущность встала на паузу.
    pub pause_time: Option<Duration>,
}

impl MetaData {
    pub fn initialized(&self) -> bool {
        self.stage >= LifeStage::Initialized
    }

    pub fn map_initialized(&self) -> bool {
        self.stage == LifeStage::MapInitialized
    }

    /// Сущность удаляется или удалена.
    pub fn terminating(&self) -> bool {
        self.stage >= LifeStage::Terminating
    }

    pub fn paused(&self) -> bool {
        self.pause_time.is_some()
    }
}

/// Маркер сущности на паузе (её карта заморожена). Системы, продвигающие симуляцию,
/// пропускают такие сущности фильтром [`Active`].
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Paused;

/// Фильтр запроса «сущность не на паузе» — поведение `EntityQueryEnumerator` Robust.
pub type Active = bevy_ecs::query::Without<Paused>;
