//! Математика Astrocore. Образец — `Robust.Shared.Maths` @ 1afd692: те же соглашения об углах
//! и направлениях и та же запись значений в данных, чтобы прототипы и карты SS14 читались
//! без изменений.
//!
//! Векторы — [`glam`]: `Vec2` читается из записи SS14 `"0.5,0.5"` и из списка `[0.5, 0.5]`.

mod named_colors;

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use std::fmt;

pub use glam::{IVec2, Vec2};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Угол в радианах. Ноль смотрит на юг (вниз по Y), положительное направление — против
/// часовой стрелки, как `Angle` Robust.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Angle(pub f32);

impl Angle {
    pub const ZERO: Angle = Angle(0.0);

    pub const fn radians(theta: f32) -> Self {
        Self(theta)
    }

    pub fn degrees(degrees: f32) -> Self {
        Self(degrees.to_radians())
    }

    pub fn to_degrees(self) -> f32 {
        self.0.to_degrees()
    }

    /// Угол направления в мире: вектор «на юг» даёт ноль.
    pub fn from_world_vec(dir: Vec2) -> Self {
        Self(dir.y.atan2(dir.x) + FRAC_PI_2)
    }

    /// Единичный вектор направления в мире: ноль — на юг.
    pub fn to_world_vec(self) -> Vec2 {
        let (sin, cos) = (self.0 - FRAC_PI_2).sin_cos();
        Vec2::new(cos, sin)
    }

    /// Поворачивает вектор против часовой стрелки.
    pub fn rotate(self, vec: Vec2) -> Vec2 {
        if self.0 == 0.0 {
            return vec;
        }
        let (sin, cos) = self.0.sin_cos();
        Vec2::new(cos * vec.x - sin * vec.y, sin * vec.x + cos * vec.y)
    }

    /// Тот же угол в пределах `(-π, π]`.
    pub fn reduced(self) -> Self {
        let mut theta = self.0 % TAU;
        if theta > PI {
            theta -= TAU;
        } else if theta <= -PI {
            theta += TAU;
        }
        Self(theta)
    }

    /// Ближайшее из восьми направлений.
    pub fn direction(self) -> Direction {
        let theta = self.0.rem_euclid(TAU);
        Direction::from_index(((theta + FRAC_PI_4 / 2.0) / FRAC_PI_4) as u8)
    }

    /// Ближайшее из четырёх основных направлений.
    pub fn cardinal_direction(self) -> Direction {
        let theta = self.0.rem_euclid(TAU);
        Direction::from_index(((theta + FRAC_PI_4) / FRAC_PI_2) as u8 * 2)
    }
}

impl std::ops::Add for Angle {
    type Output = Angle;
    fn add(self, rhs: Angle) -> Angle {
        Angle(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Angle {
    type Output = Angle;
    fn sub(self, rhs: Angle) -> Angle {
        Angle(self.0 - rhs.0)
    }
}

impl std::ops::Neg for Angle {
    type Output = Angle;
    fn neg(self) -> Angle {
        Angle(-self.0)
    }
}

impl Serialize for Angle {
    /// Запись SS14: `1.5707964 rad`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Двоичные форматы (сеть) хранят радианы числом.
        if !serializer.is_human_readable() {
            return serializer.serialize_f32(self.0);
        }
        serializer.collect_str(&format_args!("{} rad", self.0))
    }
}

impl<'de> Deserialize<'de> for Angle {
    /// `1.57 rad` — радианы; число без единицы — градусы (`AngleSerializer` Robust).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AngleVisitor;

        impl Visitor<'_> for AngleVisitor {
            type Value = Angle;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("угол: `1.57 rad` или градусы числом")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Angle, E> {
                let text = text.trim();
                let (number, radians) = match text.strip_suffix("rad") {
                    Some(number) => (number.trim(), true),
                    None => (text, false),
                };
                let value: f64 = number
                    .parse()
                    .map_err(|_| E::custom(format!("неверный угол `{text}`")))?;
                Ok(if radians {
                    Angle(value as f32)
                } else {
                    Angle::degrees(value as f32)
                })
            }

            fn visit_f64<E: de::Error>(self, degrees: f64) -> Result<Angle, E> {
                Ok(Angle::degrees(degrees as f32))
            }

            fn visit_i64<E: de::Error>(self, degrees: i64) -> Result<Angle, E> {
                Ok(Angle::degrees(degrees as f32))
            }

            fn visit_u64<E: de::Error>(self, degrees: u64) -> Result<Angle, E> {
                Ok(Angle::degrees(degrees as f32))
            }
        }

        if !deserializer.is_human_readable() {
            return f32::deserialize(deserializer).map(Angle);
        }
        deserializer.deserialize_any(AngleVisitor)
    }
}

/// Восемь направлений в порядке Robust: юг — 0, далее против часовой стрелки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum Direction {
    #[default]
    South = 0,
    SouthEast = 1,
    East = 2,
    NorthEast = 3,
    North = 4,
    NorthWest = 5,
    West = 6,
    SouthWest = 7,
}

impl Direction {
    pub const ALL: [Direction; 8] = [
        Direction::South,
        Direction::SouthEast,
        Direction::East,
        Direction::NorthEast,
        Direction::North,
        Direction::NorthWest,
        Direction::West,
        Direction::SouthWest,
    ];

    pub const CARDINAL: [Direction; 4] = [
        Direction::South,
        Direction::East,
        Direction::North,
        Direction::West,
    ];

    pub const fn from_index(index: u8) -> Self {
        Self::ALL[(index & 7) as usize]
    }

    pub const fn opposite(self) -> Self {
        Self::from_index(self as u8 + 4)
    }

    /// Поворот на 90° по часовой стрелке.
    pub const fn clockwise(self) -> Self {
        Self::from_index(self as u8 + 6)
    }

    /// Поворот на 90° против часовой стрелки.
    pub const fn counter_clockwise(self) -> Self {
        Self::from_index(self as u8 + 2)
    }

    pub const fn is_cardinal(self) -> bool {
        (self as u8).is_multiple_of(2)
    }

    /// Угол направления в пределах `(-π, π]`.
    pub fn angle(self) -> Angle {
        Angle(FRAC_PI_4 * f32::from(self as u8)).reduced()
    }

    /// Смещение на одну клетку.
    pub const fn int_vec(self) -> IVec2 {
        match self {
            Direction::South => IVec2::new(0, -1),
            Direction::SouthEast => IVec2::new(1, -1),
            Direction::East => IVec2::new(1, 0),
            Direction::NorthEast => IVec2::new(1, 1),
            Direction::North => IVec2::new(0, 1),
            Direction::NorthWest => IVec2::new(-1, 1),
            Direction::West => IVec2::new(-1, 0),
            Direction::SouthWest => IVec2::new(-1, -1),
        }
    }

    /// Единичный вектор направления.
    pub fn vec(self) -> Vec2 {
        self.int_vec().as_vec2().normalize()
    }

    /// Ближайшее направление вектора.
    pub fn from_vec(vec: Vec2) -> Self {
        Angle::from_world_vec(vec).direction()
    }
}

/// Прямоугольник со сторонами вдоль осей. Запись SS14: `"left,bottom,right,top"`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Box2(pub f32, pub f32, pub f32, pub f32);

impl Box2 {
    pub const fn new(left: f32, bottom: f32, right: f32, top: f32) -> Self {
        Self(left, bottom, right, top)
    }

    /// Прямоугольник по двум углам в любом порядке.
    pub fn from_corners(a: Vec2, b: Vec2) -> Self {
        Self(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }

    pub fn centered(center: Vec2, size: Vec2) -> Self {
        let half = size / 2.0;
        Self::from_corners(center - half, center + half)
    }

    pub const fn left(&self) -> f32 {
        self.0
    }

    pub const fn bottom(&self) -> f32 {
        self.1
    }

    pub const fn right(&self) -> f32 {
        self.2
    }

    pub const fn top(&self) -> f32 {
        self.3
    }

    pub fn min(&self) -> Vec2 {
        Vec2::new(self.0, self.1)
    }

    pub fn max(&self) -> Vec2 {
        Vec2::new(self.2, self.3)
    }

    pub fn size(&self) -> Vec2 {
        self.max() - self.min()
    }

    pub fn center(&self) -> Vec2 {
        (self.min() + self.max()) / 2.0
    }

    pub fn contains(&self, point: Vec2) -> bool {
        point.x >= self.0 && point.x <= self.2 && point.y >= self.1 && point.y <= self.3
    }

    pub fn intersects(&self, other: &Box2) -> bool {
        self.0 <= other.2 && self.2 >= other.0 && self.1 <= other.3 && self.3 >= other.1
    }

    /// Наименьший прямоугольник, вмещающий оба.
    pub fn union(&self, other: &Box2) -> Box2 {
        Box2(
            self.0.min(other.0),
            self.1.min(other.1),
            self.2.max(other.2),
            self.3.max(other.3),
        )
    }

    pub fn translated(&self, offset: Vec2) -> Box2 {
        Box2(
            self.0 + offset.x,
            self.1 + offset.y,
            self.2 + offset.x,
            self.3 + offset.y,
        )
    }

    /// Прямоугольник, расширенный на `amount` во все стороны.
    pub fn enlarged(&self, amount: f32) -> Box2 {
        Box2(
            self.0 - amount,
            self.1 - amount,
            self.2 + amount,
            self.3 + amount,
        )
    }
}

/// Цвет RGBA, компоненты от 0 до 1. Запись SS14: `"#RRGGBB"`, `"#RRGGBBAA"`, `"#RGB"`,
/// `"#RGBA"` или имя цвета (`Red`, `cornflowerblue`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Default for Color {
    fn default() -> Self {
        Self::WHITE
    }
}

impl Color {
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);
    pub const BLACK: Color = Color::rgba(0.0, 0.0, 0.0, 1.0);
    pub const TRANSPARENT: Color = Color::rgba(1.0, 1.0, 1.0, 0.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_bytes([r, g, b, a]: [u8; 4]) -> Self {
        Self::rgba(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            f32::from(a) / 255.0,
        )
    }

    pub fn to_bytes(self) -> [u8; 4] {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [byte(self.r), byte(self.g), byte(self.b), byte(self.a)]
    }

    /// Цвет из записи `#RRGGBBAA`, `#RRGGBB`, `#RGBA` или `#RGB`.
    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if !hex.is_ascii() {
            return None;
        }
        let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let single = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|v| v * 17);
        let bytes = match hex.len() {
            8 => [pair(0)?, pair(2)?, pair(4)?, pair(6)?],
            6 => [pair(0)?, pair(2)?, pair(4)?, 255],
            4 => [single(0)?, single(1)?, single(2)?, single(3)?],
            3 => [single(0)?, single(1)?, single(2)?, 255],
            _ => return None,
        };
        Some(Self::from_bytes(bytes))
    }

    /// Цвет по имени без учёта регистра.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        named_colors::NAMED_COLORS
            .binary_search_by(|(key, _)| (*key).cmp(name.as_str()))
            .ok()
            .map(|index| Self::from_bytes(named_colors::NAMED_COLORS[index].1))
    }

    /// Цвет из записи в данных: шестнадцатеричной или по имени.
    pub fn parse(text: &str) -> Option<Self> {
        Self::from_hex(text).or_else(|| Self::from_name(text))
    }

    /// Запись `#RRGGBBAA`.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.to_bytes();
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.a = alpha;
        self
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Двоичные форматы (сеть) хранят четыре байта.
        if !serializer.is_human_readable() {
            return self.to_bytes().serialize(serializer);
        }
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = Color;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("цвет: `#RRGGBB`, `#RRGGBBAA` или имя")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Color, E> {
                Color::parse(text.trim())
                    .ok_or_else(|| E::custom(format!("неизвестный цвет `{text}`")))
            }
        }

        if !deserializer.is_human_readable() {
            return <[u8; 4]>::deserialize(deserializer).map(Color::from_bytes);
        }
        deserializer.deserialize_str(ColorVisitor)
    }
}

#[cfg(test)]
mod tests {
    use astrocore_data::{from_node, parse_ron, parse_yaml, to_node};

    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn angle_conventions_match_robust() {
        assert!(close(Angle::ZERO.to_world_vec(), Vec2::new(0.0, -1.0)));
        assert!(close(
            Angle::degrees(90.0).to_world_vec(),
            Vec2::new(1.0, 0.0)
        ));
        assert!(close(
            Angle::degrees(180.0).to_world_vec(),
            Vec2::new(0.0, 1.0)
        ));
        assert!(Angle::from_world_vec(Vec2::new(0.0, -1.0)).0.abs() < 1e-6);
        assert!(close(
            Angle::degrees(90.0).rotate(Vec2::new(1.0, 0.0)),
            Vec2::new(0.0, 1.0)
        ));
        assert!((Angle::degrees(270.0).reduced().to_degrees() + 90.0).abs() < 1e-3);
    }

    #[test]
    fn directions_round_trip_through_angles_and_vectors() {
        for dir in Direction::ALL {
            assert_eq!(dir.angle().direction(), dir);
            assert_eq!(Direction::from_vec(dir.vec()), dir);
            assert_eq!(dir.opposite().opposite(), dir);
            assert_eq!(dir.clockwise().counter_clockwise(), dir);
        }
        assert_eq!(Direction::North.clockwise(), Direction::East);
        assert_eq!(Direction::South.int_vec(), IVec2::new(0, -1));
        assert_eq!(Angle::degrees(100.0).cardinal_direction(), Direction::East);
        assert_eq!(Angle::degrees(-44.0).cardinal_direction(), Direction::South);
        assert_eq!(Angle::degrees(-46.0).cardinal_direction(), Direction::West);
        assert_eq!(Angle::degrees(44.0).direction(), Direction::SouthEast);
        assert!(Direction::North.is_cardinal() && !Direction::NorthEast.is_cardinal());
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        pos: Vec2,
        tile: IVec2,
        rot: Angle,
        bounds: Box2,
        color: Color,
        dir: Direction,
    }

    #[test]
    fn reads_ss14_notation_from_yaml_and_ron() {
        let yaml = parse_yaml(
            "pos: 0.5,-1.5\ntile: -3,4\nrot: 1.5707963267948966 rad\nbounds: \"-0.25,-0.5,0.25,0.5\"\ncolor: \"#FF000080\"\ndir: East\n",
        )
        .unwrap();
        let sample: Sample = from_node(&yaml).unwrap();
        assert_eq!(sample.pos, Vec2::new(0.5, -1.5));
        assert_eq!(sample.tile, IVec2::new(-3, 4));
        assert!((sample.rot.to_degrees() - 90.0).abs() < 1e-4);
        assert_eq!(sample.bounds, Box2::new(-0.25, -0.5, 0.25, 0.5));
        assert_eq!(sample.color.to_bytes(), [255, 0, 0, 128]);

        let ron = parse_ron(
            "(pos: (0.5, -1.5), tile: (-3, 4), rot: 90, bounds: (-0.25, -0.5, 0.25, 0.5), color: \"Red\", dir: East)",
        )
        .unwrap();
        let from_ron: Sample = from_node(&ron).unwrap();
        assert!((from_ron.rot.0 - sample.rot.0).abs() < 1e-6);
        assert_eq!(from_ron.color.to_bytes(), [255, 0, 0, 255]);

        let back: Sample = from_node(&to_node(&sample).unwrap()).unwrap();
        assert_eq!(back, sample);
    }

    #[test]
    fn colors_parse_all_robust_forms() {
        assert_eq!(
            Color::parse("#abc").unwrap().to_bytes(),
            [170, 187, 204, 255]
        );
        assert_eq!(
            Color::parse("#abcd").unwrap().to_bytes(),
            [170, 187, 204, 221]
        );
        assert_eq!(
            Color::parse("CornflowerBlue").unwrap().to_hex(),
            "#6495EDFF"
        );
        assert_eq!(Color::parse("transparent").unwrap().a, 0.0);
        assert!(Color::parse("#12").is_none());
        assert!(Color::parse("нет такого").is_none());
        assert!(named_colors::NAMED_COLORS.is_sorted_by_key(|(key, _)| *key));
    }

    #[test]
    fn box_operations() {
        let a = Box2::new(0.0, 0.0, 2.0, 1.0);
        let b = Box2::from_corners(Vec2::new(3.0, 3.0), Vec2::new(1.0, 0.5));
        assert!(a.intersects(&b) && a.contains(Vec2::new(2.0, 1.0)));
        assert_eq!(a.union(&b), Box2::new(0.0, 0.0, 3.0, 3.0));
        assert_eq!(a.center(), Vec2::new(1.0, 0.5));
        assert_eq!(a.enlarged(1.0).size(), Vec2::new(4.0, 3.0));
        assert!(!a.intersects(&a.translated(Vec2::new(5.0, 0.0))));
        assert_eq!(
            Box2::centered(Vec2::ONE, Vec2::splat(2.0)),
            Box2::new(0.0, 0.0, 2.0, 2.0)
        );
    }
}
