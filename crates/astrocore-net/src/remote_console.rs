//! Консоль игрока на сервере: клиент отправляет строку команды, сервер выполняет её от
//! имени сессии и возвращает вывод. Образец — `ServerConsoleHost` и `ClientConsoleHost`
//! Robust @ 1afd692 (`MsgConCmd`, `MsgConCmdAck`).

use astrocore_console::{ConsoleShell, ConsoleWorldExt};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::events::{NetEvent, NetEventWorldExt, NetTarget};

/// Строка команды от клиента серверу (`MsgConCmd`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCommand {
    pub line: String,
}

/// Вывод команды сервера клиенту (`MsgConCmdAck`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemoteCommandOutput {
    pub output: Vec<String>,
    pub errors: Vec<String>,
}

/// Строка журнала консоли клиента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub text: String,
    pub error: bool,
}

/// Журнал консоли клиента: введённые команды и ответы сервера. Ресурс `World` клиента.
#[derive(Resource, Debug, Default)]
pub struct ConsoleLog {
    pub lines: Vec<ConsoleLine>,
    /// Растёт с каждой новой строкой: интерфейс перерисовывается, когда число меняется.
    pub version: u64,
}

/// Сколько строк хранит журнал.
pub const CONSOLE_LOG_LINES: usize = 500;

impl ConsoleLog {
    pub fn push(&mut self, text: impl Into<String>, error: bool) {
        self.lines.push(ConsoleLine {
            text: text.into(),
            error,
        });
        if self.lines.len() > CONSOLE_LOG_LINES {
            let extra = self.lines.len() - CONSOLE_LOG_LINES;
            self.lines.drain(..extra);
        }
        self.version += 1;
    }
}

/// Консоль игрока на `World` клиента.
pub trait RemoteConsoleWorldExt {
    /// Отправляет строку команды серверу и пишет её в журнал.
    fn execute_remote_command(&mut self, line: &str);
}

impl RemoteConsoleWorldExt for World {
    fn execute_remote_command(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.get_resource_or_insert_with(ConsoleLog::default)
            .push(format!("> {line}"), false);
        self.send_net_event_to_server(&RemoteCommand {
            line: line.to_owned(),
        });
    }
}

/// Сервер выполняет команду от имени сессии: права проверяет `Console::permission`.
fn execute(world: &mut World, event: &mut NetEvent<RemoteCommand>) {
    let Some(session) = event.session else {
        return;
    };
    let mut shell = ConsoleShell::player(session);
    if let Err(error) = world.execute_command(&mut shell, &event.event.line) {
        shell.write_error(error.to_string());
    }
    let reply = RemoteCommandOutput {
        output: shell.output,
        errors: shell.errors,
    };
    world.send_net_event(NetTarget::Session(session), &reply);
}

fn receive_output(world: &mut World, event: &mut NetEvent<RemoteCommandOutput>) {
    let mut log = world.get_resource_or_insert_with(ConsoleLog::default);
    for line in &event.event.output {
        log.push(line.clone(), false);
    }
    for line in &event.event.errors {
        log.push(line.clone(), true);
    }
}

pub(crate) fn register(world: &mut World) {
    world.register_net_event::<RemoteCommand>();
    world.register_net_event::<RemoteCommandOutput>();
}

pub(crate) fn register_server(world: &mut World) {
    world.subscribe_net_event::<RemoteCommand>(execute);
}

pub(crate) fn register_client(world: &mut World) {
    world.init_resource::<ConsoleLog>();
    world.subscribe_net_event::<RemoteCommandOutput>(receive_output);
}
