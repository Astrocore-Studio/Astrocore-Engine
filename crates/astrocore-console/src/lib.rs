//! Настройки (CVar) и консольные команды Astrocore по образцу `Robust.Shared/Configuration`
//! и `Robust.Shared/Console`.

mod config;
mod console;

use astrocore_ecs::GamePlugin;
use bevy_ecs::world::World;

pub use config::{
    CVarChanged, CVarDef, CVarFlags, CVarInfo, CVarOwned, CVarType, CVarValue, Config, ConfigError,
    ConfigWorldExt,
};
pub use console::{
    CommandError, CommandFn, CompletionFn, Console, ConsoleCommand, ConsoleShell, ConsoleWorldExt,
    PermissionFn, parse_arguments,
};

/// Подключает настройки и консоль с командами `help` и `cvar`.
pub struct ConsolePlugin;

impl GamePlugin for ConsolePlugin {
    fn build(&self, world: &mut World) {
        world.init_resource::<Config>();
        world.init_resource::<Console>();
        console::register(world);
    }
}
