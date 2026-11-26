//! Данные спрайта сущности. Образец — `SpriteComponent` Robust @ 1afd692: поля и их имена
//! в данных — как в прототипах SS14 (`- type: Sprite`).

use std::collections::HashMap;
use std::fmt;
use std::sync::{OnceLock, RwLock};

use astrocore_math::{Angle, Color, Vec2};
use bevy_ecs::component::Component;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn depths() -> &'static RwLock<HashMap<String, i32>> {
    static DEPTHS: OnceLock<RwLock<HashMap<String, i32>>> = OnceLock::new();
    DEPTHS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Регистрирует имя глубины отрисовки: так его понимает поле `drawdepth` спрайта в
/// прототипах (`drawdepth: Mobs`). Набор глубин задаёт игра (`DrawDepth` в SS14).
pub fn register_draw_depth(name: &str, depth: i32) {
    if let Ok(mut map) = depths().write() {
        map.insert(name.to_owned(), depth);
    }
}

/// Глубина отрисовки по имени константы или числу (`DrawDepth.X`).
pub fn draw_depth_named(name: &str) -> Option<DrawDepth> {
    if let Ok(depth) = name.trim().parse() {
        return Some(DrawDepth(depth));
    }
    depths()
        .read()
        .ok()
        .and_then(|map| map.get(name.trim()).copied())
        .map(DrawDepth)
}

/// Глубина отрисовки: большее значение рисуется поверх меньшего. В данных — имя или число.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct DrawDepth(pub i32);

impl Serialize for DrawDepth {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.0)
    }
}

impl<'de> Deserialize<'de> for DrawDepth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DepthVisitor;

        impl Visitor<'_> for DepthVisitor {
            type Value = DrawDepth;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("глубина отрисовки: имя или число")
            }

            fn visit_i64<E: de::Error>(self, depth: i64) -> Result<DrawDepth, E> {
                Ok(DrawDepth(depth as i32))
            }

            fn visit_u64<E: de::Error>(self, depth: u64) -> Result<DrawDepth, E> {
                Ok(DrawDepth(depth as i32))
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<DrawDepth, E> {
                if let Ok(depth) = name.trim().parse() {
                    return Ok(DrawDepth(depth));
                }
                depths()
                    .read()
                    .ok()
                    .and_then(|map| map.get(name.trim()).copied())
                    .map(DrawDepth)
                    .ok_or_else(|| E::custom(format!("неизвестная глубина отрисовки `{name}`")))
            }
        }

        if !deserializer.is_human_readable() {
            return i32::deserialize(deserializer).map(DrawDepth);
        }
        deserializer.deserialize_any(DepthVisitor)
    }
}

fn yes() -> bool {
    true
}

fn one() -> Vec2 {
    Vec2::ONE
}

/// Слой спрайта: состояние набора RSI или отдельная картинка.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteLayer {
    /// Набор RSI слоя; без него берётся набор спрайта.
    #[serde(default)]
    pub sprite: Option<String>,
    /// Состояние набора.
    #[serde(default)]
    pub state: Option<String>,
    /// Отдельная картинка вместо состояния набора.
    #[serde(default)]
    pub texture: Option<String>,
    #[serde(default)]
    pub color: Color,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default = "one")]
    pub scale: Vec2,
    #[serde(default)]
    pub offset: Vec2,
    #[serde(default)]
    pub rotation: Angle,
    #[serde(default)]
    pub shader: Option<String>,
    /// Ключи, по которым слой находят системы внешнего вида.
    #[serde(default)]
    pub map: Vec<String>,
    /// Направление набора RSI, которое слой показывает всегда (0 — юг, 1 — север,
    /// 2 — восток, 3 — запад), вместо направления сущности. Так сглаживание стен берёт из
    /// набора нужный угол.
    #[serde(default)]
    pub direction: Option<u8>,
    /// Номер запуска анимации состояния: при каждом увеличении анимация начинается с
    /// первого кадра (`AnimationTrackSpriteFlick`, `LayerSetAnimationTime(0)`). Ноль —
    /// анимация идёт по общим часам.
    #[serde(skip)]
    pub animation_restart: u32,
}

impl Default for SpriteLayer {
    fn default() -> Self {
        Self {
            sprite: None,
            state: None,
            texture: None,
            color: Color::WHITE,
            visible: true,
            scale: Vec2::ONE,
            offset: Vec2::ZERO,
            rotation: Angle::ZERO,
            shader: None,
            map: Vec::new(),
            direction: None,
            animation_restart: 0,
        }
    }
}

/// Спрайт сущности. В данных — компонент `Sprite`.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sprite {
    /// Набор RSI относительно каталога `Textures`.
    #[serde(default)]
    pub sprite: Option<String>,
    /// Состояние набора, если слои не заданы.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub layers: Vec<SpriteLayer>,
    #[serde(default)]
    pub drawdepth: DrawDepth,
    #[serde(default)]
    pub color: Color,
    #[serde(default = "one")]
    pub scale: Vec2,
    #[serde(default)]
    pub offset: Vec2,
    #[serde(default)]
    pub rotation: Angle,
    /// Спрайт не поворачивается вместе с сущностью.
    #[serde(default)]
    pub no_rot: bool,
    #[serde(default = "yes")]
    pub visible: bool,
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            sprite: None,
            state: None,
            layers: Vec::new(),
            drawdepth: DrawDepth::default(),
            color: Color::WHITE,
            scale: Vec2::ONE,
            offset: Vec2::ZERO,
            rotation: Angle::ZERO,
            no_rot: false,
            visible: true,
        }
    }
}

/// Источник картинки слоя.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LayerSource {
    /// Состояние набора RSI: путь набора относительно каталога ресурсов и имя состояния.
    Rsi { rsi: String, state: String },
    /// Отдельная картинка: путь относительно каталога ресурсов.
    Texture(String),
}

/// Слой, готовый к отрисовке.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLayer {
    pub source: LayerSource,
    /// Направление набора RSI, заданное слоем.
    pub direction: Option<u8>,
    pub color: Color,
    pub scale: Vec2,
    pub offset: Vec2,
    pub rotation: Angle,
    /// Номер слоя в спрайте.
    pub index: usize,
    /// Номер запуска анимации слоя ([`SpriteLayer::animation_restart`]).
    pub animation_restart: u32,
}

/// Путь ресурса картинки: пути спрайтов в данных отсчитываются от каталога `Textures`.
fn texture_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.starts_with("Textures/") {
        trimmed.to_owned()
    } else {
        format!("Textures/{trimmed}")
    }
}

fn multiply(a: Color, b: Color) -> Color {
    Color::rgba(a.r * b.r, a.g * b.g, a.b * b.b, a.a * b.a)
}

impl Sprite {
    /// Видимые слои снизу вверх. Слой без состояния и картинки — заготовка для систем
    /// внешнего вида и не рисуется.
    pub fn resolved_layers(&self) -> Vec<ResolvedLayer> {
        if !self.visible {
            return Vec::new();
        }
        let base = |source: LayerSource| ResolvedLayer {
            source,
            direction: None,
            color: self.color,
            scale: self.scale,
            offset: self.offset,
            rotation: self.rotation,
            index: 0,
            animation_restart: 0,
        };
        if self.layers.is_empty() {
            return match (&self.sprite, &self.state) {
                (Some(rsi), Some(state)) => vec![base(LayerSource::Rsi {
                    rsi: texture_path(rsi),
                    state: state.clone(),
                })],
                _ => Vec::new(),
            };
        }
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.visible)
            .filter_map(|(index, layer)| {
                let source = match (&layer.texture, &layer.state) {
                    (Some(texture), _) => LayerSource::Texture(texture_path(texture)),
                    (None, Some(state)) => LayerSource::Rsi {
                        rsi: texture_path(layer.sprite.as_ref().or(self.sprite.as_ref())?),
                        state: state.clone(),
                    },
                    (None, None) => return None,
                };
                Some(ResolvedLayer {
                    source,
                    direction: layer.direction,
                    color: multiply(self.color, layer.color),
                    scale: self.scale * layer.scale,
                    offset: self.offset + layer.offset,
                    rotation: self.rotation + layer.rotation,
                    index,
                    animation_restart: layer.animation_restart,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use astrocore_data::{from_node, parse_yaml};

    use super::*;

    #[test]
    fn single_state_sprite() {
        register_draw_depth("Walls", 2);
        let sprite: Sprite = from_node(
            &parse_yaml(
                "sprite: Structures/Walls/solid.rsi\nstate: full\ndrawdepth: Walls\nnoRot: true\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(sprite.drawdepth, DrawDepth(2));
        assert!(sprite.no_rot);
        assert_eq!(
            sprite.resolved_layers(),
            [ResolvedLayer {
                source: LayerSource::Rsi {
                    rsi: "Textures/Structures/Walls/solid.rsi".into(),
                    state: "full".into()
                },
                direction: None,
                color: Color::WHITE,
                scale: Vec2::ONE,
                offset: Vec2::ZERO,
                rotation: Angle::ZERO,
                index: 0,
                animation_restart: 0,
            }]
        );
        assert!(from_node::<Sprite>(&parse_yaml("drawdepth: Nowhere").unwrap()).is_err());
    }

    #[test]
    fn layers_inherit_and_combine_with_sprite() {
        let sprite: Sprite = from_node(
            &parse_yaml(
                r##"
sprite: Structures/Doors/airlock.rsi
color: "#808080"
scale: 2,2
offset: 0,0.5
layers:
- state: closed
  map: ["enum.DoorVisualLayers.Base"]
- state: bolted
  visible: false
- map: ["enum.DoorVisualLayers.BaseUnlit"]
- sprite: Effects/spark.rsi
  state: sparks
  color: "#FF000080"
  scale: 0.5,0.5
  offset: 1,0
- texture: /Textures/Interface/icon.png
"##,
            )
            .unwrap(),
        )
        .unwrap();
        let layers = sprite.resolved_layers();
        assert_eq!(layers.len(), 3);
        assert_eq!(
            layers[0].source,
            LayerSource::Rsi {
                rsi: "Textures/Structures/Doors/airlock.rsi".into(),
                state: "closed".into()
            }
        );
        assert_eq!(
            layers[1].source,
            LayerSource::Rsi {
                rsi: "Textures/Effects/spark.rsi".into(),
                state: "sparks".into()
            }
        );
        assert_eq!(layers[1].scale, Vec2::ONE);
        assert_eq!(layers[1].offset, Vec2::new(1.0, 0.5));
        assert_eq!(layers[1].color.to_bytes(), [128, 0, 0, 128]);
        assert_eq!(
            layers[2].source,
            LayerSource::Texture("Textures/Interface/icon.png".into())
        );

        let hidden = Sprite {
            visible: false,
            ..sprite
        };
        assert!(hidden.resolved_layers().is_empty());
        assert!(Sprite::default().resolved_layers().is_empty());
    }
}
