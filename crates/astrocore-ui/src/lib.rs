//! Интерфейсы сущностей по образцу `SharedUserInterfaceSystem` Robust @ 1afd692: сервер
//! открывает игроку окно сущности, держит его состояние и принимает сообщения окна;
//! клиент показывает окно и отправляет действия игрока.
//!
//! Крейт не рисует. Открытые окна клиента лежат в ресурсе [`OpenUis`]; показывает их слой
//! окон `astrocore-client`.

use std::collections::{BTreeMap, HashMap, HashSet};

use astrocore_ecs::{
    EntityTerminating, EventWorldExt, GamePlugin, GameWorldExt, LifecycleWorldExt,
    RegistryWorldExt, Update, component_name,
};
use astrocore_map::TransformWorldExt;
use astrocore_net::{
    Actor, ClientNet, NetEntities, NetEntity, NetEvent, NetEventWorldExt, NetTarget, Session,
};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

fn default_range() -> f32 {
    2.0
}

/// Описание одного интерфейса сущности (`InterfaceData` Robust).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceData {
    /// Вид окна на клиенте: имя класса интерфейса SS14 (`PaperBoundUserInterface`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Дальше этого расстояния окно закрывается; ноль и меньше — без ограничения.
    #[serde(default = "default_range")]
    pub interaction_range: f32,
}

/// Интерфейсы сущности. В данных — компонент `UserInterface` с полем `interfaces`:
/// ключ интерфейса (`enum.PaperUiKey.Key`) → описание.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserInterface {
    #[serde(default)]
    pub interfaces: BTreeMap<String, InterfaceData>,
    /// Сущности игроков, у которых открыт интерфейс.
    #[serde(skip)]
    actors: BTreeMap<String, Vec<Entity>>,
    /// Последнее состояние интерфейса.
    #[serde(skip)]
    states: BTreeMap<String, Vec<u8>>,
}

impl UserInterface {
    /// Игроки, у которых открыт интерфейс.
    pub fn actors(&self, key: &str) -> &[Entity] {
        self.actors.get(key).map_or(&[], Vec::as_slice)
    }
}

/// Сообщения сервера клиенту.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiServerMessage {
    Opened {
        entity: NetEntity,
        key: String,
        kind: String,
    },
    Closed {
        entity: NetEntity,
        key: String,
    },
    State {
        entity: NetEntity,
        key: String,
        state: Vec<u8>,
    },
}

/// Сообщения клиента серверу.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiClientMessage {
    /// Игрок закрыл окно.
    Close { entity: NetEntity, key: String },
    /// Действие игрока в окне: имя типа сообщения и его данные.
    Message {
        entity: NetEntity,
        key: String,
        name: String,
        bytes: Vec<u8>,
    },
}

/// Интерфейс открыт игроку. Поднимается на сущности интерфейса (сервер).
#[derive(Debug, Clone)]
pub struct BoundUiOpened {
    pub entity: Entity,
    pub key: String,
    pub actor: Entity,
}

/// Интерфейс закрыт у игрока. Поднимается на сущности интерфейса (сервер).
#[derive(Debug, Clone)]
pub struct BoundUiClosed {
    pub entity: Entity,
    pub key: String,
    pub actor: Entity,
}

/// Сообщение окна от игрока. Поднимается на сущности интерфейса (сервер).
#[derive(Debug, Clone)]
pub struct BoundUiMessage<M> {
    pub entity: Entity,
    pub key: String,
    pub actor: Entity,
    pub message: M,
}

type Dispatch = fn(&mut World, Entity, &str, Entity, &[u8]);

/// Типы сообщений окон и сущности с открытыми интерфейсами. Ресурс `World`.
#[derive(Resource, Default)]
pub struct UiRegistry {
    messages: HashMap<&'static str, Dispatch>,
    /// Сущности, у которых интерфейс открыт хотя бы одному игроку.
    active: HashSet<Entity>,
}

/// Открытое окно клиента.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenUi {
    pub entity: Entity,
    pub key: String,
    pub kind: String,
    /// Последнее состояние от сервера.
    pub state: Option<Vec<u8>>,
}

impl OpenUi {
    /// Состояние окна как значение типа `T`.
    pub fn state<T: DeserializeOwned>(&self) -> Option<T> {
        postcard::from_bytes(self.state.as_deref()?).ok()
    }
}

/// Окна, открытые сервером этому клиенту. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct OpenUis {
    pub list: Vec<OpenUi>,
    /// Растёт при каждом изменении списка или состояния: слой окон сверяет его со своим.
    pub revision: u64,
}

fn session_of(world: &World, actor: Entity) -> Option<Entity> {
    world.get::<Actor>(actor).map(|actor| actor.session)
}

fn send_to_actor(world: &mut World, actor: Entity, message: &UiServerMessage) {
    if let Some(session) = session_of(world, actor) {
        world.send_net_event(NetTarget::Session(session), message);
    }
}

/// Интерфейсы на `World`.
pub trait UiWorldExt {
    /// Регистрирует тип сообщения окна: полученное сообщение поднимается на сущности
    /// событием [`BoundUiMessage<M>`].
    fn register_ui_message<M: DeserializeOwned + 'static>(&mut self);

    /// Сервер: открывает интерфейс сущности игроку. Возвращает `false`, если такого
    /// интерфейса нет, игрок не в игре или окно уже открыто.
    fn open_ui(&mut self, entity: Entity, key: &str, actor: Entity) -> bool;

    /// Сервер: закрывает интерфейс у игрока.
    fn close_ui(&mut self, entity: Entity, key: &str, actor: Entity) -> bool;

    /// Сервер: закрывает интерфейс у всех.
    fn close_ui_for_all(&mut self, entity: Entity, key: &str);

    /// Сервер: задаёт состояние интерфейса; оно уходит всем, у кого окно открыто, и тем,
    /// кто откроет его позже.
    fn set_ui_state<T: Serialize>(&mut self, entity: Entity, key: &str, state: &T);

    /// Интерфейс открыт у игрока.
    fn ui_open_for(&self, entity: Entity, key: &str, actor: Entity) -> bool;

    /// Клиент: отправляет серверу действие игрока в окне.
    fn send_ui_message<M: Serialize + 'static>(&mut self, entity: Entity, key: &str, message: &M);

    /// Клиент: просит сервер закрыть окно.
    fn request_close_ui(&mut self, entity: Entity, key: &str);
}

impl UiWorldExt for World {
    fn register_ui_message<M: DeserializeOwned + 'static>(&mut self) {
        fn dispatch<M: DeserializeOwned + 'static>(
            world: &mut World,
            entity: Entity,
            key: &str,
            actor: Entity,
            bytes: &[u8],
        ) {
            if let Ok(message) = postcard::from_bytes::<M>(bytes) {
                world.raise(
                    entity,
                    &mut BoundUiMessage {
                        entity,
                        key: key.to_owned(),
                        actor,
                        message,
                    },
                );
            }
        }
        self.get_resource_or_insert_with(UiRegistry::default)
            .messages
            .insert(component_name::<M>(), dispatch::<M>);
    }

    fn open_ui(&mut self, entity: Entity, key: &str, actor: Entity) -> bool {
        if session_of(self, actor).is_none() {
            return false;
        }
        let Some(mut ui) = self.get_mut::<UserInterface>(entity) else {
            return false;
        };
        let Some(kind) = ui.interfaces.get(key).map(|data| data.kind.clone()) else {
            return false;
        };
        let actors = ui.actors.entry(key.to_owned()).or_default();
        if actors.contains(&actor) {
            return false;
        }
        actors.push(actor);
        let state = ui.states.get(key).cloned();
        let Some(net) = self.get::<NetEntity>(entity).copied() else {
            return false;
        };
        self.resource_mut::<UiRegistry>().active.insert(entity);
        let key = key.to_owned();
        send_to_actor(
            self,
            actor,
            &UiServerMessage::Opened {
                entity: net,
                key: key.clone(),
                kind,
            },
        );
        if let Some(state) = state {
            let message = UiServerMessage::State {
                entity: net,
                key: key.clone(),
                state,
            };
            send_to_actor(self, actor, &message);
        }
        self.raise(entity, &mut BoundUiOpened { entity, key, actor });
        true
    }

    fn close_ui(&mut self, entity: Entity, key: &str, actor: Entity) -> bool {
        let Some(mut ui) = self.get_mut::<UserInterface>(entity) else {
            return false;
        };
        let Some(actors) = ui.actors.get_mut(key) else {
            return false;
        };
        let before = actors.len();
        actors.retain(|other| *other != actor);
        if actors.len() == before {
            return false;
        }
        if ui.actors.values().all(Vec::is_empty) {
            self.resource_mut::<UiRegistry>().active.remove(&entity);
        }
        let key = key.to_owned();
        if let Some(net) = self.get::<NetEntity>(entity).copied() {
            let message = UiServerMessage::Closed {
                entity: net,
                key: key.clone(),
            };
            send_to_actor(self, actor, &message);
        }
        self.raise(entity, &mut BoundUiClosed { entity, key, actor });
        true
    }

    fn close_ui_for_all(&mut self, entity: Entity, key: &str) {
        let actors = self
            .get::<UserInterface>(entity)
            .map(|ui| ui.actors(key).to_vec())
            .unwrap_or_default();
        for actor in actors {
            self.close_ui(entity, key, actor);
        }
    }

    fn set_ui_state<T: Serialize>(&mut self, entity: Entity, key: &str, state: &T) {
        let Ok(bytes) = postcard::to_allocvec(state) else {
            return;
        };
        let Some(mut ui) = self.get_mut::<UserInterface>(entity) else {
            return;
        };
        if !ui.interfaces.contains_key(key) || ui.states.get(key) == Some(&bytes) {
            return;
        }
        ui.states.insert(key.to_owned(), bytes.clone());
        let actors = ui.actors(key).to_vec();
        let Some(net) = self.get::<NetEntity>(entity).copied() else {
            return;
        };
        let message = UiServerMessage::State {
            entity: net,
            key: key.to_owned(),
            state: bytes,
        };
        for actor in actors {
            send_to_actor(self, actor, &message);
        }
    }

    fn ui_open_for(&self, entity: Entity, key: &str, actor: Entity) -> bool {
        self.get::<UserInterface>(entity)
            .is_some_and(|ui| ui.actors(key).contains(&actor))
    }

    fn send_ui_message<M: Serialize + 'static>(&mut self, entity: Entity, key: &str, message: &M) {
        let (Some(net), Ok(bytes)) = (
            self.get::<NetEntity>(entity).copied(),
            postcard::to_allocvec(message),
        ) else {
            return;
        };
        self.send_net_event_to_server(&UiClientMessage::Message {
            entity: net,
            key: key.to_owned(),
            name: component_name::<M>().to_owned(),
            bytes,
        });
    }

    fn request_close_ui(&mut self, entity: Entity, key: &str) {
        if let Some(net) = self.get::<NetEntity>(entity).copied() {
            self.send_net_event_to_server(&UiClientMessage::Close {
                entity: net,
                key: key.to_owned(),
            });
        }
    }
}

/// Сервер: сообщение клиента. Действует, только если окно открыто у этого игрока.
fn receive_from_client(world: &mut World, event: &mut NetEvent<UiClientMessage>) {
    let Some(actor) = event
        .session
        .and_then(|session| world.get::<Session>(session)?.attached)
    else {
        return;
    };
    let (net, key) = match &event.event {
        UiClientMessage::Close { entity, key } | UiClientMessage::Message { entity, key, .. } => {
            (*entity, key.clone())
        }
    };
    let Some(entity) = world
        .get_resource::<NetEntities>()
        .and_then(|all| all.entity(net))
    else {
        return;
    };
    if !world.ui_open_for(entity, &key, actor) {
        return;
    }
    match &event.event {
        UiClientMessage::Close { .. } => {
            world.close_ui(entity, &key, actor);
        }
        UiClientMessage::Message { name, bytes, .. } => {
            let dispatch = world
                .resource::<UiRegistry>()
                .messages
                .get(name.as_str())
                .copied();
            if let Some(dispatch) = dispatch {
                dispatch(world, entity, &key, actor, bytes);
            }
        }
    }
}

/// Клиент: сообщение сервера.
fn receive_from_server(world: &mut World, event: &mut NetEvent<UiServerMessage>) {
    if event.session.is_some() {
        return;
    }
    let (net, key) = match &event.event {
        UiServerMessage::Opened { entity, key, .. }
        | UiServerMessage::Closed { entity, key }
        | UiServerMessage::State { entity, key, .. } => (*entity, key.clone()),
    };
    let Some(entity) = world
        .get_resource::<NetEntities>()
        .and_then(|all| all.entity(net))
    else {
        return;
    };
    let mut open = world.get_resource_or_insert_with(OpenUis::default);
    open.revision += 1;
    let position = open
        .list
        .iter()
        .position(|ui| ui.entity == entity && ui.key == key);
    match (&event.event, position) {
        (UiServerMessage::Opened { kind, .. }, None) => open.list.push(OpenUi {
            entity,
            key,
            kind: kind.clone(),
            state: None,
        }),
        (UiServerMessage::Closed { .. }, Some(index)) => {
            open.list.remove(index);
        }
        (UiServerMessage::State { state, .. }, Some(index)) => {
            open.list[index].state = Some(state.clone());
        }
        _ => {}
    }
}

/// Сервер: закрывает окна игроков, которые отошли, вышли или исчезли.
fn close_out_of_range(world: &mut World) {
    let active: Vec<Entity> = world
        .resource::<UiRegistry>()
        .active
        .iter()
        .copied()
        .collect();
    for entity in active {
        let Some(ui) = world.get::<UserInterface>(entity) else {
            world.resource_mut::<UiRegistry>().active.remove(&entity);
            continue;
        };
        let here = world.map_coordinates(entity);
        let mut closing: Vec<(String, Entity)> = Vec::new();
        for (key, actors) in &ui.actors {
            let range = ui
                .interfaces
                .get(key)
                .map_or(0.0, |data| data.interaction_range);
            for actor in actors {
                let near = world.is_alive(*actor)
                    && session_of(world, *actor).is_some()
                    && (range <= 0.0
                        || match (here, world.map_coordinates(*actor)) {
                            (Some(a), Some(b)) => {
                                a.map == b.map
                                    && a.z == b.z
                                    && a.position.distance(b.position) <= range
                            }
                            _ => false,
                        });
                if !near {
                    closing.push((key.clone(), *actor));
                }
            }
        }
        for (key, actor) in closing {
            world.close_ui(entity, &key, actor);
        }
    }
}

/// Интерфейсы сущностей: одинаково подключается на сервере и клиенте.
pub struct UiPlugin;

impl GamePlugin for UiPlugin {
    fn build(&self, world: &mut World) {
        world.init_resource::<UiRegistry>();
        world.init_resource::<OpenUis>();
        world.register_data_component::<UserInterface>();
        world.register_net_event::<UiServerMessage>();
        world.register_net_event::<UiClientMessage>();
        world.subscribe_net_event::<UiClientMessage>(receive_from_client);
        world.subscribe_net_event::<UiServerMessage>(receive_from_server);
        // Удаляемая сущность закрывает свои окна у игроков.
        world.subscribe::<UserInterface, EntityTerminating>(|world, entity, _| {
            let keys: Vec<String> = world
                .get::<UserInterface>(entity)
                .map(|ui| ui.actors.keys().cloned().collect())
                .unwrap_or_default();
            for key in keys {
                world.close_ui_for_all(entity, &key);
            }
        });
        // На клиенте открытых сервером окон нет: система ничего не находит.
        if world.get_resource::<ClientNet>().is_none() {
            world.add_game_systems(Update, close_out_of_range);
        }
    }
}

#[cfg(test)]
mod tests;
