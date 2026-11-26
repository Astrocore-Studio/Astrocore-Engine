//! Реестр сетевых компонентов: какие компоненты передаются клиентам и как.
//! Образец — `[NetworkedComponent]` и `ComponentState` Robust @ 1afd692.

use std::collections::HashMap;

use astrocore_ecs::{ComponentRegistry, GameTick, LifecycleWorldExt, component_name};
use bevy_ecs::component::{Component, ComponentId, Mutable};
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Ошибка разбора сетевых данных.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("повреждённые данные: {0}")]
    Decode(String),
    #[error("неизвестный сетевой компонент {0}")]
    UnknownComponent(u16),
    #[error("неизвестное сетевое событие {0}")]
    UnknownEvent(u16),
    #[error("состояние ссылается на неизвестную сущность {0}")]
    UnknownEntity(u32),
    #[error("сообщение неизвестного вида {0}")]
    UnknownMessage(u8),
}

impl From<postcard::Error> for NetError {
    fn from(error: postcard::Error) -> Self {
        NetError::Decode(error.to_string())
    }
}

/// Для кого и с какого тика пишется состояние.
#[derive(Debug, Clone, Copy)]
pub struct WriteContext {
    /// Тик, на который у клиента уже есть эта сущность; `GameTick::ZERO` — клиент её не знает.
    pub since: GameTick,
    /// Сессия-получатель.
    pub session: Entity,
}

/// Компонент с собственным сетевым состоянием: ссылки на сущности, частичные состояния.
pub trait NetComponent: Component<Mutability = Mutable> + Sized {
    /// Имя в реестре: одинаковое у сервера и клиента.
    const NAME: &'static str;
    /// Состояние ссылается на сущности, которые могут прийти в том же сообщении позже:
    /// оно применяется после создания всех сущностей сообщения.
    const DEFERRED: bool = false;

    /// Пишет состояние компонента для клиента.
    fn write(&self, world: &World, entity: Entity, ctx: &WriteContext, out: &mut Vec<u8>);

    /// Применяет состояние на клиенте: создаёт компонент или обновляет его.
    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError>;
}

type WriteFn = fn(&World, Entity, &WriteContext, &mut Vec<u8>) -> bool;
type ApplyFn = fn(&mut World, Entity, &[u8]) -> Result<(), NetError>;

/// Запись реестра об одном сетевом компоненте.
#[derive(Clone)]
pub struct NetComponentInfo {
    pub name: &'static str,
    pub component_id: ComponentId,
    /// Применяется после создания всех сущностей сообщения.
    pub deferred: bool,
    write: WriteFn,
    apply: ApplyFn,
}

impl NetComponentInfo {
    /// Пишет состояние; `false` — у сущности нет компонента.
    pub fn write(
        &self,
        world: &World,
        entity: Entity,
        ctx: &WriteContext,
        out: &mut Vec<u8>,
    ) -> bool {
        (self.write)(world, entity, ctx, out)
    }

    pub fn apply(&self, world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        (self.apply)(world, entity, bytes)
    }
}

/// Сетевые компоненты мира. Сетевой номер компонента — его место в списке по алфавиту имён,
/// поэтому сервер и клиент с одним набором компонентов нумеруют их одинаково. Ресурс `World`.
#[derive(Resource, Default)]
pub struct NetRegistry {
    /// По алфавиту имён.
    list: Vec<NetComponentInfo>,
    by_component: HashMap<ComponentId, u16>,
}

impl NetRegistry {
    fn insert(&mut self, info: NetComponentInfo) {
        if self.list.iter().any(|existing| existing.name == info.name) {
            return;
        }
        let index = self
            .list
            .partition_point(|existing| existing.name < info.name);
        self.list.insert(index, info);
        self.by_component = self
            .list
            .iter()
            .enumerate()
            .map(|(id, info)| (info.component_id, id as u16))
            .collect();
    }

    pub fn get(&self, net_id: u16) -> Option<&NetComponentInfo> {
        self.list.get(usize::from(net_id))
    }

    /// Сетевой номер компонента.
    pub fn net_id(&self, component: ComponentId) -> Option<u16> {
        self.by_component.get(&component).copied()
    }

    /// Компоненты с их сетевыми номерами.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &NetComponentInfo)> {
        self.list
            .iter()
            .enumerate()
            .map(|(id, info)| (id as u16, info))
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Отпечаток набора компонентов: сервер и клиент сверяют его при входе.
    pub fn fingerprint(&self) -> u64 {
        // FNV-1a по именам в порядке номеров.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for info in &self.list {
            for byte in info.name.bytes().chain([0]) {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }
}

fn write_custom<T: NetComponent>(
    world: &World,
    entity: Entity,
    ctx: &WriteContext,
    out: &mut Vec<u8>,
) -> bool {
    match world.get::<T>(entity) {
        Some(component) => {
            component.write(world, entity, ctx, out);
            true
        }
        None => false,
    }
}

fn write_serde<T: Component + Serialize>(
    world: &World,
    entity: Entity,
    _: &WriteContext,
    out: &mut Vec<u8>,
) -> bool {
    let Some(component) = world.get::<T>(entity) else {
        return false;
    };
    match postcard::to_extend(component, std::mem::take(out)) {
        Ok(bytes) => {
            *out = bytes;
            true
        }
        Err(_) => false,
    }
}

fn apply_serde<T: Component<Mutability = Mutable> + DeserializeOwned>(
    world: &mut World,
    entity: Entity,
    bytes: &[u8],
) -> Result<(), NetError> {
    let value: T = postcard::from_bytes(bytes)?;
    match world.get_mut::<T>(entity) {
        Some(mut component) => *component = value,
        None => {
            world.add_comp(entity, value);
        }
    }
    Ok(())
}

/// Регистрация сетевых компонентов на `World`.
pub trait NetRegistryWorldExt {
    /// Компонент с собственным состоянием.
    fn register_net_component<T: NetComponent>(&mut self);

    /// Компонент, состояние которого — он сам целиком (без ссылок на сущности).
    /// Имя берётся из реестра компонентов данных или из имени типа.
    fn register_net_serde<T>(&mut self)
    where
        T: Component<Mutability = Mutable> + Serialize + DeserializeOwned;
}

impl NetRegistryWorldExt for World {
    fn register_net_component<T: NetComponent>(&mut self) {
        let component_id = self.register_component::<T>();
        crate::sync::track_changes::<T>(self);
        self.get_resource_or_insert_with(NetRegistry::default)
            .insert(NetComponentInfo {
                name: T::NAME,
                component_id,
                deferred: T::DEFERRED,
                write: write_custom::<T>,
                apply: T::apply,
            });
    }

    fn register_net_serde<T>(&mut self)
    where
        T: Component<Mutability = Mutable> + Serialize + DeserializeOwned,
    {
        let component_id = self.register_component::<T>();
        // Компонент данных называется в сети так же, как в прототипах.
        let name: &'static str = match self
            .get_resource::<ComponentRegistry>()
            .and_then(|registry| registry.get_by_id(component_id))
        {
            Some(info) => Box::leak(info.name.to_string().into_boxed_str()),
            None => component_name::<T>(),
        };
        crate::sync::track_changes::<T>(self);
        self.get_resource_or_insert_with(NetRegistry::default)
            .insert(NetComponentInfo {
                name,
                component_id,
                deferred: false,
                write: write_serde::<T>,
                apply: apply_serde::<T>,
            });
    }
}
