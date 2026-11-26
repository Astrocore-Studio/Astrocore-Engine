//! Консольные команды. Образец — `Robust.Shared/Console` и `CommandParsing.cs` @ 1afd692.

use std::collections::BTreeMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use crate::config::{CVarFlags, Config, ConfigWorldExt};

/// Кто выполняет команду и куда идёт её вывод (`IConsoleShell` Robust).
#[derive(Debug, Default)]
pub struct ConsoleShell {
    /// Сущность-сессия игрока; `None` — консоль сервера.
    pub player: Option<Entity>,
    /// Строки вывода команды.
    pub output: Vec<String>,
    /// Строки ошибок команды.
    pub errors: Vec<String>,
}

impl ConsoleShell {
    /// Консоль сервера: ей разрешены все команды.
    pub fn server() -> Self {
        Self::default()
    }

    pub fn player(player: Entity) -> Self {
        Self {
            player: Some(player),
            ..Self::default()
        }
    }

    pub fn is_server(&self) -> bool {
        self.player.is_none()
    }

    pub fn write_line(&mut self, line: impl Into<String>) {
        self.output.push(line.into());
    }

    pub fn write_error(&mut self, line: impl Into<String>) {
        self.errors.push(line.into());
    }
}

/// Действие команды: мир, оболочка, аргументы без имени команды.
pub type CommandFn = fn(&mut World, &mut ConsoleShell, &[String]);
/// Подсказки для аргумента, который сейчас набирается (последний в списке).
pub type CompletionFn = fn(&World, &ConsoleShell, &[String]) -> Vec<String>;

/// Консольная команда (`IConsoleCommand` Robust).
#[derive(Clone)]
pub struct ConsoleCommand {
    pub name: &'static str,
    pub description: &'static str,
    /// Строка использования: `tp <x> <y> [карта]`.
    pub help: &'static str,
    pub execute: CommandFn,
    pub complete: Option<CompletionFn>,
}

/// Проверка прав: может ли оболочка выполнить команду с таким именем.
pub type PermissionFn = fn(&World, &ConsoleShell, &str) -> bool;

/// Команды мира. Ресурс `World`.
#[derive(Resource, Default)]
pub struct Console {
    commands: BTreeMap<&'static str, ConsoleCommand>,
    /// Проверка прав игроков; задаёт игра. Без неё игрокам команды запрещены.
    pub permission: Option<PermissionFn>,
}

impl Console {
    /// Регистрирует команду; команда с тем же именем заменяется.
    pub fn register(&mut self, command: ConsoleCommand) {
        self.commands.insert(command.name, command);
    }

    pub fn get(&self, name: &str) -> Option<&ConsoleCommand> {
        self.commands.get(name)
    }

    /// Команды по алфавиту.
    pub fn iter(&self) -> impl Iterator<Item = &ConsoleCommand> {
        self.commands.values()
    }
}

/// Делит строку команды на аргументы: пробелы разделяют, кавычки объединяют,
/// обратная косая черта экранирует следующий символ.
pub fn parse_arguments(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => current.push(chars.next().unwrap_or('\\')),
            '"' => {
                if in_quotes {
                    args.push(std::mem::take(&mut current));
                }
                in_quotes = !in_quotes;
            }
            ' ' | '\t' | '\n' if !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("пустая команда")]
    Empty,
    #[error("неизвестная команда {0}")]
    Unknown(String),
    #[error("нет прав на команду {0}")]
    Forbidden(String),
}

/// Консоль на `World`.
pub trait ConsoleWorldExt {
    /// Регистрирует команду.
    fn register_command(&mut self, command: ConsoleCommand);

    /// Выполняет строку команды от имени оболочки.
    fn execute_command(&mut self, shell: &mut ConsoleShell, line: &str)
    -> Result<(), CommandError>;

    /// Подсказки для набираемой строки: имена команд или аргументы.
    fn complete_command(&self, shell: &ConsoleShell, line: &str) -> Vec<String>;
}

fn allowed(world: &World, shell: &ConsoleShell, name: &str) -> bool {
    shell.is_server()
        || world
            .get_resource::<Console>()
            .and_then(|console| console.permission)
            .is_some_and(|check| check(world, shell, name))
}

impl ConsoleWorldExt for World {
    fn register_command(&mut self, command: ConsoleCommand) {
        self.get_resource_or_insert_with(Console::default)
            .register(command);
    }

    fn execute_command(
        &mut self,
        shell: &mut ConsoleShell,
        line: &str,
    ) -> Result<(), CommandError> {
        let args = parse_arguments(line);
        let Some((name, rest)) = args.split_first() else {
            return Err(CommandError::Empty);
        };
        let command = self
            .get_resource::<Console>()
            .and_then(|console| console.get(name))
            .cloned()
            .ok_or_else(|| CommandError::Unknown(name.clone()))?;
        if !allowed(self, shell, command.name) {
            return Err(CommandError::Forbidden(name.clone()));
        }
        (command.execute)(self, shell, rest);
        Ok(())
    }

    fn complete_command(&self, shell: &ConsoleShell, line: &str) -> Vec<String> {
        let Some(console) = self.get_resource::<Console>() else {
            return Vec::new();
        };
        let mut args = parse_arguments(line);
        if line.ends_with([' ', '\t']) || args.is_empty() {
            args.push(String::new());
        }
        let (name, rest) = args.split_first().expect("хотя бы один аргумент");
        if rest.is_empty() {
            return console
                .iter()
                .filter(|c| c.name.starts_with(name.as_str()) && allowed(self, shell, c.name))
                .map(|c| c.name.to_owned())
                .collect();
        }
        let Some(command) = console.get(name).filter(|c| allowed(self, shell, c.name)) else {
            return Vec::new();
        };
        let prefix = rest.last().map(String::as_str).unwrap_or_default();
        command.complete.map_or_else(Vec::new, |complete| {
            complete(self, shell, rest)
                .into_iter()
                .filter(|option| option.starts_with(prefix))
                .collect()
        })
    }
}

fn help_command(world: &mut World, shell: &mut ConsoleShell, args: &[String]) {
    let Some(console) = world.get_resource::<Console>() else {
        return;
    };
    match args.first() {
        Some(name) => match console.get(name) {
            Some(command) => {
                shell.write_line(format!("{} — {}", command.name, command.description));
                shell.write_line(format!("Использование: {}", command.help));
            }
            None => shell.write_error(format!("Неизвестная команда {name}.")),
        },
        None => {
            let lines: Vec<String> = console
                .iter()
                .map(|command| format!("{} — {}", command.name, command.description))
                .collect();
            lines.into_iter().for_each(|line| shell.write_line(line));
        }
    }
}

fn command_names(world: &World, _: &ConsoleShell, args: &[String]) -> Vec<String> {
    if args.len() > 1 {
        return Vec::new();
    }
    world
        .get_resource::<Console>()
        .map(|console| console.iter().map(|c| c.name.to_owned()).collect())
        .unwrap_or_default()
}

fn cvar_command(world: &mut World, shell: &mut ConsoleShell, args: &[String]) {
    let Some(name) = args.first() else {
        shell.write_error("Использование: cvar <имя> [значение]");
        return;
    };
    if let Some(value) = args.get(1)
        && let Err(error) = world.set_cvar_text(name, value)
    {
        shell.write_error(error.to_string());
        return;
    }
    match world.get_resource::<Config>().and_then(|c| c.info(name)) {
        Some(info) if info.flags.contains(CVarFlags::CONFIDENTIAL) => {
            shell.write_line(format!("{name} = (скрыто)"));
        }
        Some(info) => {
            shell.write_line(format!("{name} = {}", info.value));
            if args.len() == 1 && !info.description.is_empty() {
                shell.write_line(info.description.to_owned());
            }
        }
        None => shell.write_error(format!("Настройка {name} не зарегистрирована.")),
    }
}

fn cvar_names(world: &World, _: &ConsoleShell, args: &[String]) -> Vec<String> {
    if args.len() > 1 {
        return Vec::new();
    }
    world
        .get_resource::<Config>()
        .map(|config| config.iter().map(|info| info.name.to_owned()).collect())
        .unwrap_or_default()
}

/// Регистрирует встроенные команды `help` и `cvar`.
pub(crate) fn register(world: &mut World) {
    world.register_command(ConsoleCommand {
        name: "help",
        description: "Список команд или справка по команде.",
        help: "help [команда]",
        execute: help_command,
        complete: Some(command_names),
    });
    world.register_command(ConsoleCommand {
        name: "cvar",
        description: "Показывает или меняет настройку.",
        help: "cvar <имя> [значение]",
        execute: cvar_command,
        complete: Some(cvar_names),
    });
}

#[cfg(test)]
mod tests {
    use astrocore_ecs::GameWorldExt;

    use super::*;
    use crate::ConsolePlugin;
    use crate::config::CVarDef;

    const TICKRATE: CVarDef<i32> =
        CVarDef::new("net.tickrate", 30, CVarFlags::SERVER, "Тиков в секунду.");
    const PASSWORD: CVarDef<&str> = CVarDef::new(
        "game.password",
        "",
        CVarFlags::CONFIDENTIAL,
        "Пароль сервера.",
    );

    #[test]
    fn arguments_follow_robust_parsing() {
        assert_eq!(parse_arguments("tp 1 2"), ["tp", "1", "2"]);
        assert_eq!(
            parse_arguments("say \"hello world\"  \"\" a\\ b \\\"q"),
            ["say", "hello world", "", "a b", "\"q"]
        );
        assert!(parse_arguments("   ").is_empty());
        assert_eq!(parse_arguments("end\\"), ["end\\"]);
    }

    fn world() -> World {
        let mut world = World::new();
        world.add_plugin(ConsolePlugin);
        world.register_cvar(&TICKRATE);
        world.register_cvar(&PASSWORD);
        world
    }

    #[test]
    fn builtin_commands_work_from_server_console() {
        let mut world = world();
        let mut shell = ConsoleShell::server();
        world
            .execute_command(&mut shell, "cvar net.tickrate 60")
            .unwrap();
        assert_eq!(shell.output, ["net.tickrate = 60"]);
        assert_eq!(world.cvar(&TICKRATE), 60);

        let mut shell = ConsoleShell::server();
        world
            .execute_command(&mut shell, "cvar net.tickrate")
            .unwrap();
        assert_eq!(shell.output, ["net.tickrate = 60", "Тиков в секунду."]);
        world
            .execute_command(&mut shell, "cvar game.password secret")
            .unwrap();
        assert_eq!(shell.output.last().unwrap(), "game.password = (скрыто)");
        world
            .execute_command(&mut shell, "cvar net.tickrate many")
            .unwrap();
        world.execute_command(&mut shell, "cvar nope").unwrap();
        assert_eq!(shell.errors.len(), 2);

        let mut shell = ConsoleShell::server();
        world.execute_command(&mut shell, "help").unwrap();
        assert_eq!(shell.output.len(), 2);
        world.execute_command(&mut shell, "help cvar").unwrap();
        assert_eq!(
            shell.output.last().unwrap(),
            "Использование: cvar <имя> [значение]"
        );

        assert_eq!(
            world.execute_command(&mut shell, "fly"),
            Err(CommandError::Unknown("fly".into()))
        );
        assert_eq!(
            world.execute_command(&mut shell, " "),
            Err(CommandError::Empty)
        );
    }

    #[test]
    fn players_need_permission() {
        let mut world = world();
        let player = world.spawn_empty().id();
        let mut shell = ConsoleShell::player(player);
        assert_eq!(
            world.execute_command(&mut shell, "help"),
            Err(CommandError::Forbidden("help".into()))
        );
        assert!(world.complete_command(&shell, "he").is_empty());

        world.resource_mut::<Console>().permission = Some(|_, _, name| name == "help");
        world.execute_command(&mut shell, "help").unwrap();
        assert_eq!(
            world.execute_command(&mut shell, "cvar net.tickrate 1"),
            Err(CommandError::Forbidden("cvar".into()))
        );
        assert_eq!(world.complete_command(&shell, ""), ["help"]);
    }

    #[test]
    fn completion_offers_commands_and_arguments() {
        let world = world();
        let shell = ConsoleShell::server();
        assert_eq!(world.complete_command(&shell, "c"), ["cvar"]);
        assert_eq!(world.complete_command(&shell, ""), ["cvar", "help"]);
        assert_eq!(
            world.complete_command(&shell, "cvar net."),
            ["net.tickrate"]
        );
        assert_eq!(
            world.complete_command(&shell, "cvar "),
            ["game.password", "net.tickrate"]
        );
        assert!(
            world
                .complete_command(&shell, "cvar net.tickrate ")
                .is_empty()
        );
        assert_eq!(world.complete_command(&shell, "help cv"), ["cvar"]);
    }
}
