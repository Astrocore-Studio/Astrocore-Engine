//! Компоненты физики: тело и его фикстуры. Образец — `PhysicsComponent`, `FixturesComponent`
//! и `Fixture` Robust @ 1afd692; поля и их имена в данных — как в прототипах SS14.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{OnceLock, RwLock};

use astrocore_math::Vec2;
use bevy_ecs::component::Component;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::shapes::Shape;

fn groups() -> &'static RwLock<HashMap<String, i32>> {
    static GROUPS: OnceLock<RwLock<HashMap<String, i32>>> = OnceLock::new();
    GROUPS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Регистрирует имя группы столкновений: так его понимают поля `layer` и `mask` фикстур
/// в прототипах (`- MobMask`). Набор групп задаёт игра (`CollisionGroup` в SS14).
pub fn register_collision_group(name: &str, bits: i32) {
    if let Ok(mut map) = groups().write() {
        map.insert(name.to_owned(), bits);
    }
}

fn group_bits(name: &str) -> Option<i32> {
    groups().read().ok()?.get(name).copied()
}

/// Набор групп столкновений: биты слоя или маски фикстуры. В данных — имя группы, число
/// или их список.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CollisionGroups(pub i32);

impl CollisionGroups {
    pub const NONE: CollisionGroups = CollisionGroups(0);
    pub const ALL: CollisionGroups = CollisionGroups(-1);

    pub const fn intersects(self, other: CollisionGroups) -> bool {
        self.0 & other.0 != 0
    }
}

impl Serialize for CollisionGroups {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.0)
    }
}

impl<'de> Deserialize<'de> for CollisionGroups {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct GroupsVisitor;

        /// Имя группы, число или несколько имён через запятую (`MidImpassable, LowImpassable`).
        fn named<E: de::Error>(text: &str) -> Result<i32, E> {
            let mut bits = 0;
            for name in text
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                bits |= match name.parse::<i32>() {
                    Ok(value) => value,
                    Err(_) => group_bits(name).ok_or_else(|| {
                        E::custom(format!("неизвестная группа столкновений `{name}`"))
                    })?,
                };
            }
            Ok(bits)
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Item {
            Bits(i32),
            Name(String),
        }

        impl<'de> Visitor<'de> for GroupsVisitor {
            type Value = CollisionGroups;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("группа столкновений, число или их список")
            }

            fn visit_i64<E: de::Error>(self, bits: i64) -> Result<CollisionGroups, E> {
                Ok(CollisionGroups(bits as i32))
            }

            fn visit_u64<E: de::Error>(self, bits: u64) -> Result<CollisionGroups, E> {
                Ok(CollisionGroups(bits as i32))
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<CollisionGroups, E> {
                named(name).map(CollisionGroups)
            }

            fn visit_unit<E: de::Error>(self) -> Result<CollisionGroups, E> {
                Ok(CollisionGroups::NONE)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<CollisionGroups, A::Error> {
                let mut bits = 0;
                while let Some(item) = seq.next_element::<Item>()? {
                    bits |= match item {
                        Item::Bits(value) => value,
                        Item::Name(name) => named(&name)?,
                    };
                }
                Ok(CollisionGroups(bits))
            }
        }

        // Двоичный сетевой формат хранит биты числом; имена понимают только YAML и RON.
        if !deserializer.is_human_readable() {
            return i32::deserialize(deserializer).map(CollisionGroups);
        }
        deserializer.deserialize_any(GroupsVisitor)
    }
}

/// Вид тела (`BodyType` Robust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyType {
    /// Движется заданной скоростью, столкновения его не останавливают.
    Kinematic,
    /// Неподвижно: стены, привязанные к клетке объекты.
    #[default]
    Static,
    /// Движется под действием импульсов и столкновений.
    Dynamic,
    /// Персонаж: скорость задаёт управление, стены и предметы его останавливают.
    KinematicController,
}

impl BodyType {
    /// Тело реагирует на столкновения.
    pub const fn is_movable(self) -> bool {
        matches!(self, BodyType::Dynamic | BodyType::KinematicController)
    }
}

/// Положение относительно пола (`BodyStatus` Robust): в воздухе трение о пол не действует.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyStatus {
    #[default]
    OnGround,
    InAir,
}

fn yes() -> bool {
    true
}

fn default_damping() -> f32 {
    0.2
}

/// Физическое тело. В данных — компонент `Physics`.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Physics {
    #[serde(default)]
    pub body_type: BodyType,
    #[serde(default)]
    pub body_status: BodyStatus,
    /// Тело участвует в столкновениях.
    #[serde(default = "yes")]
    pub can_collide: bool,
    /// Столкновения не вращают тело.
    #[serde(default = "yes")]
    pub fixed_rotation: bool,
    /// Затухание скорости: доля в секунду.
    #[serde(default = "default_damping")]
    pub linear_damping: f32,
    #[serde(default = "default_damping")]
    pub angular_damping: f32,
    /// Покоящееся тело может уснуть и выпасть из расчёта.
    #[serde(default = "yes")]
    pub sleeping_allowed: bool,
    /// Скорость в тайлах в секунду в координатах грида или карты.
    #[serde(default)]
    pub linear_velocity: Vec2,
    /// Угловая скорость в радианах в секунду.
    #[serde(default)]
    pub angular_velocity: f32,
    /// Тело участвует в расчёте.
    #[serde(default)]
    pub awake: bool,
    /// Сколько секунд тело почти неподвижно.
    #[serde(skip)]
    pub sleep_time: f32,
    /// Масса по фикстурам.
    #[serde(skip)]
    pub mass: f32,
}

impl Default for Physics {
    fn default() -> Self {
        Self {
            body_type: BodyType::Static,
            body_status: BodyStatus::OnGround,
            can_collide: true,
            fixed_rotation: true,
            linear_damping: default_damping(),
            angular_damping: default_damping(),
            sleeping_allowed: true,
            linear_velocity: Vec2::ZERO,
            angular_velocity: 0.0,
            awake: false,
            sleep_time: 0.0,
            mass: 0.0,
        }
    }
}

impl Physics {
    /// Обратная масса: ноль у тел, которые столкновения не двигают.
    pub fn inv_mass(&self) -> f32 {
        if self.body_type.is_movable() && self.mass > 0.0 {
            1.0 / self.mass
        } else {
            0.0
        }
    }
}

fn default_friction() -> f32 {
    0.4
}

fn default_density() -> f32 {
    1.0
}

/// Фикстура: форма тела с правилами столкновений.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
    #[serde(default)]
    pub shape: Shape,
    /// Твёрдая фикстура останавливает тела; нетвёрдая только сообщает о касании.
    #[serde(default = "yes")]
    pub hard: bool,
    /// Группы, к которым относится фикстура.
    #[serde(default)]
    pub layer: CollisionGroups,
    /// Группы, с которыми фикстура сталкивается.
    #[serde(default)]
    pub mask: CollisionGroups,
    #[serde(default = "default_density")]
    pub density: f32,
    #[serde(default = "default_friction")]
    pub friction: f32,
    /// Упругость: 0 — без отскока.
    #[serde(default)]
    pub restitution: f32,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            shape: Shape::default(),
            hard: true,
            layer: CollisionGroups::NONE,
            mask: CollisionGroups::NONE,
            density: default_density(),
            friction: default_friction(),
            restitution: 0.0,
        }
    }
}

impl Fixture {
    /// Фикстуры сталкиваются, если маска одной пересекается со слоем другой
    /// (`ShouldCollide` Robust).
    pub fn should_collide(&self, other: &Fixture) -> bool {
        self.mask.intersects(other.layer) || other.mask.intersects(self.layer)
    }
}

/// Фикстуры тела по именам. В данных — компонент `Fixtures`.
#[derive(Component, Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fixtures {
    pub fixtures: BTreeMap<String, Fixture>,
}

impl Fixtures {
    /// Масса тела: сумма плотности на площадь по фикстурам.
    pub fn mass(&self) -> f32 {
        self.fixtures
            .values()
            .map(|fixture| fixture.density * fixture.shape.area())
            .sum()
    }
}
