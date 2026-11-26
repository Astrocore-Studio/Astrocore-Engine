//! Данные внешнего вида сущности: сервер пишет значения по ключам, клиент по ним меняет
//! спрайт. Образец — `AppearanceComponent` и `SharedAppearanceSystem` Robust @ 1afd692.

use std::collections::BTreeMap;
use std::fmt;

use astrocore_ecs::{EventWorldExt, LifecycleWorldExt};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::registry::{NetComponent, NetError, WriteContext};

/// Значение внешнего вида.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AppearanceValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Text(String),
}

impl fmt::Display for AppearanceValue {
    /// Текст значения, по которому визуализаторы ищут его в данных прототипа. Логические
    /// значения пишутся как в SS14: `True` и `False`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(true) => f.write_str("True"),
            Self::Bool(false) => f.write_str("False"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Text(value) => f.write_str(value),
        }
    }
}

impl From<bool> for AppearanceValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for AppearanceValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for AppearanceValue {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<f32> for AppearanceValue {
    fn from(value: f32) -> Self {
        Self::Float(value)
    }
}

impl From<&str> for AppearanceValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for AppearanceValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

/// Внешний вид сущности: значения по ключам. Ключ — имя как в данных SS14
/// (`enum.DoorVisuals.State`). В данных — компонент `Appearance` без полей.
#[derive(Component, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    #[serde(skip)]
    data: BTreeMap<String, AppearanceValue>,
}

impl Appearance {
    pub fn get(&self, key: &str) -> Option<&AppearanceValue> {
        self.data.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AppearanceValue)> {
        self.data.iter().map(|(key, value)| (key.as_str(), value))
    }
}

/// Внешний вид сущности изменился: визуализаторам пора обновить спрайт. Поднимается на
/// самой сущности.
#[derive(Debug, Clone, Copy)]
pub struct AppearanceChange {
    pub entity: Entity,
}

impl NetComponent for Appearance {
    const NAME: &'static str = "Appearance";

    fn write(&self, _: &World, _: Entity, _: &WriteContext, out: &mut Vec<u8>) {
        if let Ok(bytes) = postcard::to_extend(&self.data, std::mem::take(out)) {
            *out = bytes;
        }
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let data: BTreeMap<String, AppearanceValue> = postcard::from_bytes(bytes)?;
        match world.get_mut::<Appearance>(entity) {
            Some(appearance) if appearance.data == data => return Ok(()),
            Some(mut appearance) => appearance.data = data,
            None => {
                world.add_comp(entity, Appearance { data });
            }
        }
        world.raise(entity, &mut AppearanceChange { entity });
        Ok(())
    }
}

/// Внешний вид на `World`.
pub trait AppearanceWorldExt {
    /// Записывает значение. Возвращает `false`, если у сущности нет компонента `Appearance`
    /// или значение не изменилось.
    fn set_appearance(
        &mut self,
        entity: Entity,
        key: &str,
        value: impl Into<AppearanceValue>,
    ) -> bool;

    /// Убирает значение.
    fn remove_appearance(&mut self, entity: Entity, key: &str) -> bool;

    fn appearance(&self, entity: Entity, key: &str) -> Option<&AppearanceValue>;
}

impl AppearanceWorldExt for World {
    fn set_appearance(
        &mut self,
        entity: Entity,
        key: &str,
        value: impl Into<AppearanceValue>,
    ) -> bool {
        let value = value.into();
        let Some(mut appearance) = self.get_mut::<Appearance>(entity) else {
            return false;
        };
        // Компонент помечается изменённым, только если значение правда другое: иначе
        // состояние уходило бы клиентам каждый такт.
        if appearance.data.get(key) == Some(&value) {
            return false;
        }
        appearance.data.insert(key.to_owned(), value);
        self.raise(entity, &mut AppearanceChange { entity });
        true
    }

    fn remove_appearance(&mut self, entity: Entity, key: &str) -> bool {
        let Some(mut appearance) = self.get_mut::<Appearance>(entity) else {
            return false;
        };
        if !appearance.data.contains_key(key) {
            return false;
        }
        appearance.data.remove(key);
        self.raise(entity, &mut AppearanceChange { entity });
        true
    }

    fn appearance(&self, entity: Entity, key: &str) -> Option<&AppearanceValue> {
        self.get::<Appearance>(entity)?.get(key)
    }
}
