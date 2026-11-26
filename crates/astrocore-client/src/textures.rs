//! Картинки клиента: наборы RSI и отдельные текстуры из каталогов ресурсов.

use std::collections::HashMap;

use astrocore_resources::{Resources, RsiMeta, RsiState};
use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;

/// Пикселей на тайл (`EyeManager.PixelsPerMeter` Robust).
pub const PIXELS_PER_TILE: f32 = 32.0;

/// Состояние набора RSI, готовое к отрисовке.
#[derive(Debug, Clone)]
pub struct StateTexture {
    pub image: Handle<Image>,
    pub layout: Handle<TextureAtlasLayout>,
    pub state: RsiState,
    /// Размер кадра в тайлах.
    pub size: Vec2,
}

/// Загруженные картинки. Картинка читается при первом обращении; отсутствующая
/// запоминается, чтобы не искать её снова. Ресурс приложения.
#[derive(Resource, Default)]
pub struct Textures {
    metas: HashMap<String, Option<RsiMeta>>,
    states: HashMap<(String, String), Option<StateTexture>>,
    single: HashMap<String, Option<(Handle<Image>, Vec2)>>,
    /// Сколько картинок не найдено или не прочитано.
    pub missing: usize,
}

fn decode(resources: &Resources, path: &str) -> Option<Image> {
    let bytes = resources.read(path).ok()?;
    Image::from_buffer(
        &bytes,
        ImageType::Extension("png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::nearest(),
        RenderAssetUsages::default(),
    )
    .ok()
}

impl Textures {
    /// Состояние набора RSI: `rsi` — путь набора от каталога ресурсов.
    pub fn state(
        &mut self,
        resources: &Resources,
        images: &mut Assets<Image>,
        layouts: &mut Assets<TextureAtlasLayout>,
        rsi: &str,
        state: &str,
    ) -> Option<StateTexture> {
        let key = (rsi.to_owned(), state.to_owned());
        if let Some(found) = self.states.get(&key) {
            return found.clone();
        }
        let meta = self
            .metas
            .entry(rsi.to_owned())
            .or_insert_with(|| resources.rsi_meta(rsi).ok())
            .clone();
        let loaded = meta.and_then(|meta| {
            let info = meta.state(state)?.clone();
            let image = decode(resources, &format!("{rsi}/{state}.png"))?;
            let frame = UVec2::new(meta.size.0, meta.size.1);
            let columns = (image.width() / frame.x).max(1);
            let rows = (image.height() / frame.y).max(1);
            Some(StateTexture {
                image: images.add(image),
                layout: layouts.add(TextureAtlasLayout::from_grid(
                    frame, columns, rows, None, None,
                )),
                state: info,
                size: frame.as_vec2() / PIXELS_PER_TILE,
            })
        });
        if loaded.is_none() {
            self.missing += 1;
        }
        self.states.insert(key, loaded.clone());
        loaded
    }

    /// Отдельная картинка и её размер в тайлах.
    pub fn single(
        &mut self,
        resources: &Resources,
        images: &mut Assets<Image>,
        path: &str,
    ) -> Option<(Handle<Image>, Vec2)> {
        if let Some(found) = self.single.get(path) {
            return found.clone();
        }
        let loaded = decode(resources, path).map(|image| {
            let size = image.size().as_vec2() / PIXELS_PER_TILE;
            (images.add(image), size)
        });
        if loaded.is_none() {
            self.missing += 1;
        }
        self.single.insert(path.to_owned(), loaded.clone());
        loaded
    }
}

/// Направление набора RSI для угла поворота сущности. Порядок направлений в наборе:
/// юг, север, восток, запад, затем юго-восток, юго-запад, северо-восток, северо-запад.
pub fn rsi_direction(directions: usize, rotation: astrocore_math::Angle) -> usize {
    use astrocore_math::Direction;
    match directions {
        4 => match rotation.cardinal_direction() {
            Direction::North => 1,
            Direction::East => 2,
            Direction::West => 3,
            _ => 0,
        },
        8 => match rotation.direction() {
            Direction::South => 0,
            Direction::North => 1,
            Direction::East => 2,
            Direction::West => 3,
            Direction::SouthEast => 4,
            Direction::SouthWest => 5,
            Direction::NorthEast => 6,
            Direction::NorthWest => 7,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use astrocore_math::Angle;

    use super::*;

    #[test]
    fn directions_follow_rsi_order() {
        assert_eq!(rsi_direction(1, Angle::degrees(123.0)), 0);
        assert_eq!(rsi_direction(4, Angle::ZERO), 0);
        assert_eq!(rsi_direction(4, Angle::degrees(180.0)), 1);
        assert_eq!(rsi_direction(4, Angle::degrees(90.0)), 2);
        assert_eq!(rsi_direction(4, Angle::degrees(-90.0)), 3);
        assert_eq!(rsi_direction(8, Angle::degrees(45.0)), 4);
        assert_eq!(rsi_direction(8, Angle::degrees(-45.0)), 5);
        assert_eq!(rsi_direction(8, Angle::degrees(135.0)), 6);
        assert_eq!(rsi_direction(8, Angle::degrees(-135.0)), 7);
    }
}
