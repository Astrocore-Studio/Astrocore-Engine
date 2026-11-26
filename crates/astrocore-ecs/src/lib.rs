//! Ядро сущностей Astrocore по образцу Robust Toolbox (ADR-0015).
//!
//! Сущности и компоненты хранит [`bevy_ecs`]; этот крейт добавляет то, чего в нём нет:
//! жизненный цикл сущностей и компонентов, синхронные направленные события, реестр
//! компонентов данных и игровое время.

mod event;
mod game;
mod lifecycle;
mod meta;
mod registry;
mod timing;

pub use bevy_ecs;

pub use event::{BroadcastHandler, DirectedHandler, EventBus, EventWorldExt, Order};
pub use game::{GamePlugin, GameWorldExt, PostUpdate, PreUpdate, Update};
pub use lifecycle::{
    ComponentAdd, ComponentInit, ComponentRemove, ComponentShutdown, ComponentStartup,
    DeletionQueue, EntityDeleted, EntityPaused, EntityTerminating, EntityUnpaused,
    LifecycleWorldExt, MapInit,
};
pub use meta::{Active, LifeStage, MetaData, Paused};
pub use registry::{
    ComponentInfo, ComponentRegistry, ErasedComponent, ParsedComponent, RegistryWorldExt,
    component_name,
};
pub use timing::{DEFAULT_TICK_RATE, GameTick, GameTime};

/// Частые импорты для кода систем.
pub mod prelude {
    pub use bevy_ecs::prelude::*;

    pub use crate::{
        Active, ComponentAdd, ComponentInit, ComponentRemove, ComponentShutdown, ComponentStartup,
        EntityTerminating, EventWorldExt, GamePlugin, GameTick, GameTime, GameWorldExt, LifeStage,
        LifecycleWorldExt, MapInit, MetaData, Order, Paused, PostUpdate, PreUpdate,
        RegistryWorldExt, Update,
    };
}
