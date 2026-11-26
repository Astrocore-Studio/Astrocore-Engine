//! Слои видимости: сервер передаёт игроку только сущности, слой которых входит в маску его
//! глаза. Так обычные игроки не получают призраков и скрытое под полом. Образец —
//! `VisibilityComponent`, `EyeComponent.VisibilityMask` и отбор в `PvsSystem` Robust.

use std::collections::HashMap;
use std::fmt;
use std::sync::{OnceLock, RwLock};

use astrocore_ecs::{ComponentInit, ComponentShutdown, EventWorldExt, RegistryWorldExt};
use bevy_ecs::component::Component;
use bevy_ecs::entity::{Entity, EntityHashMap};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::registry::NetRegistryWorldExt;

/// Слой и маска по умолчанию: обычный видимый мир (`VisibilityFlags.Normal` SS14).
pub const DEFAULT_VISIBILITY: u32 = 1;

fn names() -> &'static RwLock<HashMap<String, u32>> {
    static NAMES: OnceLock<RwLock<HashMap<String, u32>>> = OnceLock::new();
    NAMES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Регистрирует имя слоя видимости: так его понимают данные (`visMask: [Normal, Ghost]`).
/// Набор слоёв задаёт игра (`VisibilityFlags` SS14).
pub fn register_visibility_layer(name: &str, bits: u32) {
    if let Ok(mut map) = names().write() {
        map.insert(name.to_owned(), bits);
    }
}

fn named(name: &str) -> Option<u32> {
    let name = name.trim();
    name.parse()
        .ok()
        .or_else(|| names().read().ok()?.get(name).copied())
}

/// Набор слоёв видимости. В данных — число, имя, имена через запятую или список имён.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisibilityMask(pub u32);

impl Default for VisibilityMask {
    fn default() -> Self {
        Self(DEFAULT_VISIBILITY)
    }
}

impl Serialize for VisibilityMask {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for VisibilityMask {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MaskVisitor;

        impl<'de> Visitor<'de> for MaskVisitor {
            type Value = VisibilityMask;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("слои видимости: число, имя или список имён")
            }

            fn visit_u64<E: de::Error>(self, bits: u64) -> Result<VisibilityMask, E> {
                Ok(VisibilityMask(bits as u32))
            }

            fn visit_i64<E: de::Error>(self, bits: i64) -> Result<VisibilityMask, E> {
                Ok(VisibilityMask(bits as u32))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<VisibilityMask, E> {
                text.split(',').try_fold(VisibilityMask(0), |mask, name| {
                    named(name)
                        .map(|bits| VisibilityMask(mask.0 | bits))
                        .ok_or_else(|| E::custom(format!("неизвестный слой видимости `{name}`")))
                })
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<VisibilityMask, A::Error> {
                let mut mask = 0;
                while let Some(VisibilityMask(bits)) = seq.next_element()? {
                    mask |= bits;
                }
                Ok(VisibilityMask(mask))
            }
        }

        if !deserializer.is_human_readable() {
            return u32::deserialize(deserializer).map(VisibilityMask);
        }
        deserializer.deserialize_any(MaskVisitor)
    }
}

fn yes() -> bool {
    true
}

/// Слой видимости сущности и её потомков. В данных — компонент `Visibility`. Сущность без
/// компонента лежит в обычном слое. Слой меняют через [`VisibilityWorldExt`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Visibility {
    #[serde(default)]
    pub layer: VisibilityMask,
}

/// Глаз сущности игрока: что он видит. В данных — компонент `Eye` (`EyeComponent` Robust).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eye {
    /// Невидимое из глаза закрыто чёрным; у призраков выключено.
    #[serde(default = "yes")]
    pub draw_fov: bool,
    /// Слои, которые сервер передаёт этому игроку.
    #[serde(default)]
    pub vis_mask: VisibilityMask,
}

impl Default for Eye {
    fn default() -> Self {
        Self {
            draw_fov: true,
            vis_mask: VisibilityMask::default(),
        }
    }
}

/// Сущности с необычным слоем видимости: отбор видимых сверяется только с ними, остальные
/// сущности проверки не стоят ничего. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct VisibilityLayers(EntityHashMap<u32>);

impl VisibilityLayers {
    /// Сущность видна глазу с маской. Потомки скрытой сущности тоже скрыты: отбор не
    /// спускается в них.
    pub fn visible(&self, entity: Entity, mask: u32) -> bool {
        self.0.is_empty() || self.0.get(&entity).is_none_or(|layer| layer & mask != 0)
    }
}

fn index(world: &mut World, entity: Entity) {
    let layer = world.get::<Visibility>(entity).map(|v| v.layer.0);
    let mut layers = world.get_resource_or_insert_with(VisibilityLayers::default);
    match layer {
        Some(layer) if layer != DEFAULT_VISIBILITY => {
            layers.0.insert(entity, layer);
        }
        _ => {
            layers.0.remove(&entity);
        }
    }
}

/// Слои видимости на `World`.
pub trait VisibilityWorldExt {
    /// Задаёт слой видимости сущности.
    fn set_visibility_layer(&mut self, entity: Entity, layer: u32);

    /// Маска глаза сущности: слои, которые видит управляющий ею игрок.
    fn eye_mask(&self, entity: Entity) -> u32;
}

impl VisibilityWorldExt for World {
    fn set_visibility_layer(&mut self, entity: Entity, layer: u32) {
        let Ok(mut target) = self.get_entity_mut(entity) else {
            return;
        };
        target.insert(Visibility {
            layer: VisibilityMask(layer),
        });
        index(self, entity);
    }

    fn eye_mask(&self, entity: Entity) -> u32 {
        self.get::<Eye>(entity)
            .map_or(DEFAULT_VISIBILITY, |eye| eye.vis_mask.0)
    }
}

pub(crate) fn register(world: &mut World) {
    world.init_resource::<VisibilityLayers>();
    world.register_data_component::<Visibility>();
    world.register_data_component::<Eye>();
    // Клиенту нужен глаз своей сущности: по нему он рисует поле зрения.
    world.register_net_serde::<Eye>();
    world.subscribe::<Visibility, ComponentInit>(|world, entity, _| index(world, entity));
    world.subscribe::<Visibility, ComponentShutdown>(|world, entity, _| {
        world.resource_mut::<VisibilityLayers>().0.remove(&entity);
    });
}
