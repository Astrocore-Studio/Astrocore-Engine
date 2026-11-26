//! Ввод Astrocore по образцу Robust: клавиши назначаются функциям ввода (`KeyFunctions`),
//! контексты решают, какие функции доступны, а нажатая функция уходит серверу командой с
//! точкой и сущностью под курсором.
//!
//! Крейт не знает об окне: слой окна (`astrocore-client`) переводит клавиши в имена Robust
//! и сообщает о нажатиях [`KeyBindings`].

mod binds;
mod command;

pub use binds::{
    BindKind, BoundKeyState, COMMON_CONTEXT, FunctionChange, InputContexts, KeyBind, KeyBindings,
};
pub use command::{Clickable, InputArgs, InputCommand, InputHandlers, InputPlugin, InputWorldExt};

/// Функции ввода движка (`EngineKeyFunctions` Robust): имена — как в `keybinds.yml`.
pub mod functions {
    pub const MOVE_UP: &str = "MoveUp";
    pub const MOVE_DOWN: &str = "MoveDown";
    pub const MOVE_LEFT: &str = "MoveLeft";
    pub const MOVE_RIGHT: &str = "MoveRight";
    pub const WALK: &str = "Walk";
    pub const USE: &str = "Use";
    pub const USE_SECONDARY: &str = "UseSecondary";
    pub const UI_CLICK: &str = "UIClick";
    pub const UI_RIGHT_CLICK: &str = "UIRightClick";
    pub const CLOSE_MODALS: &str = "CloseModals";
    pub const SHOW_DEBUG_MONITORS: &str = "ShowDebugMonitors";
    pub const HIDE_UI: &str = "HideUI";
    pub const SHOW_DEBUG_CONSOLE: &str = "ShowDebugConsole";
    pub const SHOW_ESCAPE_MENU: &str = "ShowEscapeMenu";
    pub const TOGGLE_FULLSCREEN: &str = "ToggleFullscreen";

    /// Функции общего контекста движка (`EngineContexts.SetupContexts`).
    pub const COMMON: [&str; 9] = [
        USE,
        USE_SECONDARY,
        UI_CLICK,
        UI_RIGHT_CLICK,
        CLOSE_MODALS,
        SHOW_DEBUG_MONITORS,
        HIDE_UI,
        SHOW_DEBUG_CONSOLE,
        SHOW_ESCAPE_MENU,
    ];

    /// Функции движения: контекст `human` движка.
    pub const MOVEMENT: [&str; 5] = [MOVE_UP, MOVE_DOWN, MOVE_LEFT, MOVE_RIGHT, WALK];
}

/// Назначения движения по умолчанию для игры без файла `keybinds.yml`.
pub const FALLBACK_KEYBINDS: &str = "binds:
- {function: MoveUp, type: State, key: W}
- {function: MoveLeft, type: State, key: A}
- {function: MoveDown, type: State, key: S}
- {function: MoveRight, type: State, key: D}
- {function: Walk, type: State, key: Shift}
- {function: Use, type: State, key: MouseLeft, canFocus: true}
- {function: UseSecondary, type: State, key: MouseRight, canFocus: true}
";

/// Контексты движка: общий и `human` с движением.
pub fn engine_contexts() -> InputContexts {
    let mut contexts = InputContexts::default();
    contexts.add_functions(COMMON_CONTEXT, functions::COMMON);
    contexts.add("human", COMMON_CONTEXT);
    contexts.add_functions("human", functions::MOVEMENT);
    contexts
}

#[cfg(test)]
mod tests;
