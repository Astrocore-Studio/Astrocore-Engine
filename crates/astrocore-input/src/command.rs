//! Команды ввода: клиент сообщает серверу, что игрок нажал функцию ввода, и куда при этом
//! указывал курсор. Образец — `FullInputCmdMessage`, `CommandBinds` и `InputSystem` Robust.

use std::collections::HashMap;
use std::sync::Arc;

use astrocore_ecs::{GamePlugin, RegistryWorldExt};
use astrocore_map::{MapCoordinates, MapWorldExt, Transform, TransformWorldExt};
use astrocore_math::Vec2;
use astrocore_net::{ClientNet, NetEntities, NetEntity, NetEvent, NetEventWorldExt, Session};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::binds::BoundKeyState;

/// Сущность можно выбрать курсором. В данных — компонент `Clickable`.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clickable {}

/// Сообщение клиента серверу о функции ввода.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputCommand {
    pub function: String,
    pub state: BoundKeyState,
    /// Точка под курсором на карте игрока.
    pub position: Option<Vec2>,
    /// Сущность под курсором.
    pub target: Option<NetEntity>,
}

/// Функция ввода, пришедшая обработчику.
#[derive(Debug, Clone, PartialEq)]
pub struct InputArgs {
    pub function: String,
    pub state: BoundKeyState,
    /// Сессия игрока на сервере; на клиенте — `None`.
    pub session: Option<Entity>,
    /// Сущность игрока.
    pub player: Option<Entity>,
    /// Точка под курсором.
    pub coordinates: Option<MapCoordinates>,
    /// Сущность под курсором.
    pub target: Option<Entity>,
}

type Handler = Arc<dyn Fn(&mut World, &InputArgs) -> bool + Send + Sync>;

/// Обработчики функций ввода (`CommandBinds` Robust). Ресурс `World`.
#[derive(Resource, Default, Clone)]
pub struct InputHandlers {
    handlers: HashMap<String, Vec<Handler>>,
}

/// Ввод на `World`.
pub trait InputWorldExt {
    /// Назначает обработчик функции ввода. Обработчик возвращает `true`, если ввод
    /// обработан: следующие обработчики функции его не получат.
    fn bind_input(
        &mut self,
        function: &str,
        handler: impl Fn(&mut World, &InputArgs) -> bool + Send + Sync + 'static,
    );

    /// Вызывает обработчики функции. Возвращает `true`, если ввод обработан.
    fn handle_input(&mut self, args: &InputArgs) -> bool;

    /// Клиент: вызывает свои обработчики функции и отправляет команду серверу.
    fn send_input(
        &mut self,
        function: &str,
        state: BoundKeyState,
        position: Option<Vec2>,
        target: Option<Entity>,
    ) -> bool;
}

/// Точка на карте и этаже сущности игрока.
fn on_player_map(world: &World, player: Option<Entity>, position: Vec2) -> Option<MapCoordinates> {
    let transform = world.get::<Transform>(player?)?;
    let map = world.map_id_of(transform.map()?)?;
    let z = world.world_transform(player?)?.level;
    Some(MapCoordinates { map, position, z })
}

impl InputWorldExt for World {
    fn bind_input(
        &mut self,
        function: &str,
        handler: impl Fn(&mut World, &InputArgs) -> bool + Send + Sync + 'static,
    ) {
        self.get_resource_or_insert_with(InputHandlers::default)
            .handlers
            .entry(function.to_owned())
            .or_default()
            .push(Arc::new(handler));
    }

    fn handle_input(&mut self, args: &InputArgs) -> bool {
        let handlers = self
            .get_resource::<InputHandlers>()
            .and_then(|all| all.handlers.get(&args.function))
            .cloned()
            .unwrap_or_default();
        handlers.iter().any(|handler| handler(self, args))
    }

    fn send_input(
        &mut self,
        function: &str,
        state: BoundKeyState,
        position: Option<Vec2>,
        target: Option<Entity>,
    ) -> bool {
        let player = self
            .get_resource::<ClientNet>()
            .and_then(|net| net.attached);
        let handled = self.handle_input(&InputArgs {
            function: function.to_owned(),
            state,
            session: None,
            player,
            coordinates: position.and_then(|point| on_player_map(self, player, point)),
            target,
        });
        let target = target.and_then(|entity| self.get::<NetEntity>(entity).copied());
        self.send_net_event_to_server(&InputCommand {
            function: function.to_owned(),
            state,
            position,
            target,
        });
        handled
    }
}

/// Самое длинное имя функции ввода, которое сервер принимает от клиента.
const MAX_FUNCTION_NAME: usize = 64;

fn receive(world: &mut World, event: &mut NetEvent<InputCommand>) {
    // Команды приходят только от клиентов.
    let Some(session) = event.session else {
        return;
    };
    let command = &event.event;
    if command.function.len() > MAX_FUNCTION_NAME
        || command.position.is_some_and(|point| !point.is_finite())
    {
        return;
    }
    let player = world.get::<Session>(session).and_then(|s| s.attached);
    let target = command
        .target
        .and_then(|net| world.get_resource::<NetEntities>()?.entity(net));
    let args = InputArgs {
        function: command.function.clone(),
        state: command.state,
        session: Some(session),
        player,
        coordinates: command
            .position
            .and_then(|point| on_player_map(world, player, point)),
        target,
    };
    world.handle_input(&args);
}

/// Команды ввода: одинаково подключается на сервере и клиенте.
pub struct InputPlugin;

impl GamePlugin for InputPlugin {
    fn build(&self, world: &mut World) {
        world.init_resource::<InputHandlers>();
        world.register_data_component::<Clickable>();
        world.register_net_event::<InputCommand>();
        world.subscribe_net_event::<InputCommand>(receive);
    }
}
