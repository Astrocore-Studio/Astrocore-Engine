//! Сетевые события (`RaiseNetworkEvent` Robust): сообщения между клиентом и сервером вне
//! состояний мира. Идут по надёжному каналу. Ссылки на сущности в событиях — [`NetEntity`].

use std::any::TypeId;
use std::collections::HashMap;

use astrocore_ecs::EventWorldExt;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::ClientNet;
use crate::protocol::{ClientMessage, ServerMessage};
use crate::registry::NetError;
use crate::server::{ServerNet, Session};

/// Полученное сетевое событие. Поднимается широковещательно.
#[derive(Debug, Clone)]
pub struct NetEvent<E> {
    /// Сессия-отправитель; `None` — событие пришло клиенту от сервера.
    pub session: Option<Entity>,
    pub event: E,
}

type DispatchFn = fn(&mut World, Option<Entity>, &[u8]) -> Result<(), NetError>;

struct EventInfo {
    name: &'static str,
    type_id: TypeId,
    dispatch: DispatchFn,
}

/// Сетевые события мира: номер события — его место по алфавиту имён типов. Ресурс `World`.
#[derive(Resource, Default)]
pub struct NetEvents {
    list: Vec<EventInfo>,
    by_type: HashMap<TypeId, u16>,
}

fn dispatch_as<E: DeserializeOwned + Send + Sync + 'static>(
    world: &mut World,
    session: Option<Entity>,
    bytes: &[u8],
) -> Result<(), NetError> {
    let event: E = postcard::from_bytes(bytes)?;
    world.raise_broadcast(&mut NetEvent { session, event });
    Ok(())
}

/// Разбирает и поднимает полученное событие. Неизвестные и повреждённые события отбрасываются.
pub(crate) fn dispatch(world: &mut World, session: Option<Entity>, id: u16, bytes: &[u8]) {
    let Some(dispatch) = world
        .get_resource::<NetEvents>()
        .and_then(|events| events.list.get(usize::from(id)))
        .map(|info| info.dispatch)
    else {
        return;
    };
    let _ = dispatch(world, session, bytes);
}

/// Кому сервер отправляет событие.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetTarget {
    /// Одной сессии.
    Session(Entity),
    /// Всем сессиям.
    All,
}

/// Сетевые события на `World`.
pub trait NetEventWorldExt {
    /// Регистрирует тип события. Наборы событий сервера и клиента должны совпадать.
    fn register_net_event<E>(&mut self)
    where
        E: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Подписывается на полученное событие: `SubscribeNetworkEvent<E>` Robust.
    fn subscribe_net_event<E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, &mut NetEvent<E>) + Send + Sync + 'static,
    );

    /// Сервер отправляет событие клиентам.
    fn send_net_event<E: Serialize + 'static>(&mut self, target: NetTarget, event: &E);

    /// Клиент отправляет событие серверу.
    fn send_net_event_to_server<E: Serialize + 'static>(&mut self, event: &E);
}

fn encode<E: Serialize + 'static>(world: &World, event: &E) -> Option<(u16, Vec<u8>)> {
    let id = *world
        .get_resource::<NetEvents>()?
        .by_type
        .get(&TypeId::of::<E>())?;
    Some((id, postcard::to_allocvec(event).ok()?))
}

impl NetEventWorldExt for World {
    fn register_net_event<E>(&mut self)
    where
        E: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let mut events = self.get_resource_or_insert_with(NetEvents::default);
        let type_id = TypeId::of::<E>();
        if events.by_type.contains_key(&type_id) {
            return;
        }
        let name = std::any::type_name::<E>();
        let index = events.list.partition_point(|info| info.name < name);
        events.list.insert(
            index,
            EventInfo {
                name,
                type_id,
                dispatch: dispatch_as::<E>,
            },
        );
        events.by_type = events
            .list
            .iter()
            .enumerate()
            .map(|(id, info)| (info.type_id, id as u16))
            .collect();
    }

    fn subscribe_net_event<E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, &mut NetEvent<E>) + Send + Sync + 'static,
    ) {
        self.subscribe_broadcast::<NetEvent<E>>(handler);
    }

    fn send_net_event<E: Serialize + 'static>(&mut self, target: NetTarget, event: &E) {
        let Some((id, bytes)) = encode(self, event) else {
            return;
        };
        let conns: Vec<_> = match target {
            NetTarget::Session(session) => self
                .get::<Session>(session)
                .map(|s| s.conn)
                .into_iter()
                .collect(),
            NetTarget::All => self
                .query::<&Session>()
                .iter(self)
                .map(|session| session.conn)
                .collect(),
        };
        let Some(mut net) = self.get_resource_mut::<ServerNet>() else {
            return;
        };
        let message = ServerMessage::Event { id, bytes };
        for conn in conns {
            net.send(conn, true, &message);
        }
    }

    fn send_net_event_to_server<E: Serialize + 'static>(&mut self, event: &E) {
        let Some((id, bytes)) = encode(self, event) else {
            return;
        };
        if let Some(mut net) = self.get_resource_mut::<ClientNet>() {
            net.send(true, &ClientMessage::Event { id, bytes });
        }
    }
}
