//! Передача настроек между сервером и клиентом. Образец — `NetConfigurationManager`,
//! `ServerNetConfigurationManager`, `ClientNetConfigurationManager` и `MsgConVars` Robust @
//! 1afd692.
//!
//! Сервер отправляет вошедшему игроку настройки с флагом `REPLICATED` (кроме `CLIENT`) и
//! рассылает всем каждое их изменение. Клиент применяет их, пропуская свои `CLIENT`, и сам
//! отправляет серверу свои `REPLICATED` без флага `SERVER` — при подключении и при каждом
//! изменении. Сервер хранит значения клиента у сессии ([`ClientCVars`]).

use std::collections::BTreeMap;

use astrocore_console::{CVarChanged, CVarDef, CVarFlags, CVarOwned, CVarType, CVarValue, Config};
use astrocore_ecs::EventWorldExt;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::events::{NetEvent, NetEventWorldExt, NetTarget};
use crate::server::{PlayerConnected, Session};

/// Значение настройки в сообщении.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CVarWire {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<&CVarValue> for CVarWire {
    fn from(value: &CVarValue) -> Self {
        match value {
            CVarValue::Bool(v) => Self::Bool(*v),
            CVarValue::Int(v) => Self::Int(*v),
            CVarValue::Float(v) => Self::Float(*v),
            CVarValue::Text(v) => Self::Text(v.clone()),
        }
    }
}

impl From<CVarWire> for CVarValue {
    fn from(value: CVarWire) -> Self {
        match value {
            CVarWire::Bool(v) => Self::Bool(v),
            CVarWire::Int(v) => Self::Int(v),
            CVarWire::Float(v) => Self::Float(v),
            CVarWire::Text(v) => Self::Text(v),
        }
    }
}

/// Настройки по имени (`MsgConVars`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CVarsMessage {
    pub vars: Vec<(String, CVarWire)>,
}

/// Значения настроек клиента у сессии на сервере (`_replicatedCVars`).
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ClientCVars(pub BTreeMap<String, CVarValue>);

/// Клиент принял приветствие сервера. Широковещательно на клиенте.
#[derive(Debug, Clone, Copy)]
pub struct ConnectedToServer;

/// Настройка передаётся с этой стороны: `REPLICATED` и не чужого флага (`GetReplicatedVars`).
fn outgoing(flags: CVarFlags, server: bool) -> bool {
    let foreign = if server {
        CVarFlags::CLIENT
    } else {
        CVarFlags::SERVER
    };
    flags.contains(CVarFlags::REPLICATED) && !flags.contains(foreign)
}

/// Передаваемые настройки этой стороны.
pub fn replicated_vars(config: &Config, server: bool) -> CVarsMessage {
    CVarsMessage {
        vars: config
            .iter()
            .filter(|info| outgoing(info.flags, server))
            .map(|info| (info.name.to_owned(), CVarWire::from(info.value)))
            .collect(),
    }
}

fn changed_message(world: &World, event: &CVarChanged, server: bool) -> Option<CVarsMessage> {
    let info = world.get_resource::<Config>()?.info(&event.name)?;
    outgoing(info.flags, server).then(|| CVarsMessage {
        vars: vec![(event.name.clone(), CVarWire::from(&event.value))],
    })
}

/// Сервер: значения клиента (`ApplyNetVarChange`) — только известные `REPLICATED`, приведённые
/// к виду настройки.
fn receive_on_server(world: &mut World, event: &mut NetEvent<CVarsMessage>) {
    let Some(session) = event.session else {
        return;
    };
    let Some(config) = world.get_resource::<Config>() else {
        return;
    };
    let accepted: Vec<(String, CVarValue)> = event
        .event
        .vars
        .iter()
        .filter_map(|(name, value)| {
            let info = config.info(name)?;
            if !info.flags.contains(CVarFlags::REPLICATED) {
                return None;
            }
            Some((name.clone(), coerce(value.clone().into(), info.default)?))
        })
        .collect();
    let Ok(mut entity) = world.get_entity_mut(session) else {
        return;
    };
    let mut values = entity.get::<ClientCVars>().cloned().unwrap_or_default();
    values.0.extend(accepted);
    entity.insert(values);
}

/// Значение к виду значения по умолчанию.
fn coerce(value: CVarValue, like: &CVarValue) -> Option<CVarValue> {
    value.coerce(like).ok()
}

/// Клиент: значения сервера, кроме своих `CLIENT` (`ApplyClientNetVarChange`).
fn receive_on_client(world: &mut World, event: &mut NetEvent<CVarsMessage>) {
    for (name, value) in event.event.vars.clone() {
        let changed = {
            let Some(mut config) = world.get_resource_mut::<Config>() else {
                return;
            };
            let Some(flags) = config.info(&name).map(|info| info.flags) else {
                continue;
            };
            if flags.contains(CVarFlags::CLIENT) {
                continue;
            }
            config.set_value(&name, value.into()).ok().flatten()
        };
        if let Some(value) = changed {
            world.raise_broadcast(&mut CVarChanged { name, value });
        }
    }
}

/// Настройки клиента на `World` сервера.
pub trait ClientCVarWorldExt {
    /// Значение настройки клиента сессии (`GetClientCVar`); без значения — по умолчанию.
    fn client_cvar<T: CVarType + CVarOwned>(&self, session: Entity, def: &CVarDef<T>) -> T;
}

impl ClientCVarWorldExt for World {
    fn client_cvar<T: CVarType + CVarOwned>(&self, session: Entity, def: &CVarDef<T>) -> T {
        self.get::<ClientCVars>(session)
            .and_then(|values| values.0.get(def.name))
            .and_then(T::from_value)
            .unwrap_or_else(|| def.default.clone())
    }
}

pub(crate) fn register(world: &mut World) {
    world.register_net_event::<CVarsMessage>();
}

pub(crate) fn register_server(world: &mut World) {
    world.subscribe_net_event::<CVarsMessage>(receive_on_server);
    // Вошедшему — все передаваемые настройки (`SyncConnectingClient`).
    world.subscribe_broadcast::<PlayerConnected>(|world, event| {
        let session = event.session;
        if world.get::<Session>(session).is_none() {
            return;
        }
        let message = match world.get_resource::<Config>() {
            Some(config) => replicated_vars(config, true),
            None => return,
        };
        world.send_net_event(NetTarget::Session(session), &message);
    });
    world.subscribe_broadcast::<CVarChanged>(|world, event| {
        if let Some(message) = changed_message(world, event, true) {
            world.send_net_event(NetTarget::All, &message);
        }
    });
}

pub(crate) fn register_client(world: &mut World) {
    world.subscribe_net_event::<CVarsMessage>(receive_on_client);
    // При подключении — свои настройки серверу (`SyncWithServer`).
    world.subscribe_broadcast::<ConnectedToServer>(|world, _| {
        let message = match world.get_resource::<Config>() {
            Some(config) => replicated_vars(config, false),
            None => return,
        };
        world.send_net_event_to_server(&message);
    });
    world.subscribe_broadcast::<CVarChanged>(|world, event| {
        if let Some(message) = changed_message(world, event, false) {
            world.send_net_event_to_server(&message);
        }
    });
}
