use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_world::AuthoredKind;
use bevy::asset::RenderAssetUsages;
use bevy::image::{
    CompressedImageFormats, ImageAddressMode, ImageFilterMode, ImageSampler,
    ImageSamplerDescriptor, ImageType,
};
use bevy::prelude::*;
use serde::Deserialize;

use crate::billboard::{SpriteSheet, spawn_billboard};
use crate::{MaterialCache, RenderedWorld};

/// Предел размера файла манифеста и картинки: данные недоверенные.
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Текстура материала: стены, полы и прочая геометрия.
#[derive(Debug, Clone, Deserialize)]
pub struct MaterialEntry {
    /// Путь к PNG относительно каталога манифеста.
    pub image: String,
}

/// Спрайт объекта-размещения (предмет на земле): направленный билборд.
#[derive(Debug, Clone, Deserialize)]
pub struct SpriteEntry {
    pub image: String,
    /// Размер в метрах: ширина и высота.
    #[serde(default = "default_sprite_size")]
    pub size: [f32; 2],
}

fn default_sprite_size() -> [f32; 2] {
    [0.5, 0.5]
}

/// Соответствие идентификаторов движка картинкам: клиент не знает о форматах игры, игровой
/// билд сам генерирует этот файл из своих прототипов и ассетов.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RenderManifest {
    #[serde(default)]
    pub materials: BTreeMap<String, MaterialEntry>,
    #[serde(default)]
    pub sprites: BTreeMap<String, SpriteEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("манифест: {0}")]
    Parse(String),
    #[error("{0}: файл больше допустимого")]
    TooLarge(PathBuf),
    #[error("{0}: {1}")]
    Image(PathBuf, String),
    #[error("небезопасный путь {0:?}")]
    BadPath(String),
}

/// Текстуры материалов по идентификатору прототипа.
#[derive(Resource, Default, Clone)]
pub struct MaterialTextures(pub HashMap<PrototypeId, Handle<Image>>);

#[derive(Clone)]
pub struct PlacementSprite {
    pub image: Handle<Image>,
    pub size: Vec2,
}

/// Спрайты размещений по идентификатору прототипа.
#[derive(Resource, Default, Clone)]
pub struct PlacementSprites(pub HashMap<PrototypeId, PlacementSprite>);

impl RenderManifest {
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let len = std::fs::metadata(path)
            .map_err(|e| ManifestError::Io(path.to_path_buf(), e))?
            .len();
        if len > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge(path.to_path_buf()));
        }
        let text =
            std::fs::read_to_string(path).map_err(|e| ManifestError::Io(path.to_path_buf(), e))?;
        serde_json::from_str(&text).map_err(|e| ManifestError::Parse(e.to_string()))
    }
}

/// Относительный путь без выхода из каталога манифеста.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, ManifestError> {
    let p = Path::new(rel);
    let ok = !rel.is_empty()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok {
        Ok(base.join(p))
    } else {
        Err(ManifestError::BadPath(rel.to_string()))
    }
}

fn load_png(path: &Path, repeat: bool) -> Result<Image, ManifestError> {
    let len = std::fs::metadata(path)
        .map_err(|e| ManifestError::Io(path.to_path_buf(), e))?
        .len();
    if len > MAX_IMAGE_BYTES {
        return Err(ManifestError::TooLarge(path.to_path_buf()));
    }
    let bytes = std::fs::read(path).map_err(|e| ManifestError::Io(path.to_path_buf(), e))?;
    let mut image = Image::from_buffer(
        &bytes,
        ImageType::Extension("png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::nearest(),
        RenderAssetUsages::default(),
    )
    .map_err(|e| ManifestError::Image(path.to_path_buf(), e.to_string()))?;
    // Пиксельная графика: без сглаживания; материалы стен и полов повторяются по плитке.
    let mode = if repeat {
        ImageAddressMode::Repeat
    } else {
        ImageAddressMode::ClampToEdge
    };
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: mode,
        address_mode_v: mode,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..default()
    });
    Ok(image)
}

impl RenderManifest {
    /// Загрузить все картинки и вернуть готовые ресурсы. Неверный идентификатор или путь — ошибка:
    /// молча терять текстуры не нужно.
    pub fn install(
        &self,
        base: &Path,
        images: &mut Assets<Image>,
    ) -> Result<(MaterialTextures, PlacementSprites), ManifestError> {
        let mut textures = MaterialTextures::default();
        for (id, e) in &self.materials {
            let pid =
                PrototypeId::new(id.as_str()).map_err(|e| ManifestError::Parse(e.to_string()))?;
            let img = load_png(&safe_join(base, &e.image)?, true)?;
            textures.0.insert(pid, images.add(img));
        }
        let mut sprites = PlacementSprites::default();
        for (id, e) in &self.sprites {
            let pid =
                PrototypeId::new(id.as_str()).map_err(|e| ManifestError::Parse(e.to_string()))?;
            let img = load_png(&safe_join(base, &e.image)?, false)?;
            if !(e.size[0] > 0.0
                && e.size[1] > 0.0
                && e.size[0].is_finite()
                && e.size[1].is_finite())
            {
                return Err(ManifestError::Parse(format!("размер спрайта {id}")));
            }
            sprites.0.insert(
                pid,
                PlacementSprite {
                    image: images.add(img),
                    size: Vec2::new(e.size[0], e.size[1]),
                },
            );
        }
        Ok((textures, sprites))
    }
}

/// Маркер билборда размещения.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementBillboard(pub ObjectId);

pub struct PlacementSpritesPlugin;

impl Plugin for PlacementSpritesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementSprites>()
            .init_resource::<MaterialTextures>()
            .add_systems(Update, (apply_material_textures, sync_placements));
    }
}

/// Передать текстуры кэшу материалов (до первой отрисовки региона).
fn apply_material_textures(textures: Res<MaterialTextures>, mut cache: ResMut<MaterialCache>) {
    if textures.is_changed() {
        cache.textures = textures.0.clone();
        cache.clear();
    }
}

/// Держит билборды в соответствии с авторскими объектами-размещениями мира.
fn sync_placements(
    mut commands: Commands,
    rendered: Option<Res<RenderedWorld>>,
    sprites: Res<PlacementSprites>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<(Entity, &PlacementBillboard)>,
    mut seen: Local<Option<(u64, usize)>>,
) {
    let Some(rendered) = rendered else { return };
    let key = (rendered.world.revision().0, sprites.0.len());
    if *seen == Some(key) {
        return;
    }
    *seen = Some(key);
    let want: HashMap<ObjectId, (Vec3, &PlacementSprite)> = rendered
        .world
        .authored()
        .filter_map(|o| {
            let AuthoredKind::Placement { prototype } = &o.kind else {
                return None;
            };
            let s = sprites.0.get(prototype)?;
            let (lo, hi) = (o.min(), o.max());
            Some((
                o.id,
                (Vec3::new((lo.x + hi.x) * 0.5, lo.y, (lo.z + hi.z) * 0.5), s),
            ))
        })
        .collect();
    let mut have: HashMap<ObjectId, Entity> = HashMap::new();
    for (e, b) in &existing {
        if want.contains_key(&b.0) && !have.contains_key(&b.0) {
            have.insert(b.0, e);
        } else {
            commands.entity(e).despawn();
        }
    }
    for (id, (feet, sprite)) in want {
        if have.contains_key(&id) {
            commands
                .entity(have[&id])
                .insert(Transform::from_translation(feet));
            continue;
        }
        let sheet = SpriteSheet {
            image: sprite.image.clone(),
            columns: 1,
        };
        let e = spawn_billboard(
            &mut commands,
            &mut meshes,
            &mut materials,
            &sheet,
            feet,
            sprite.size,
            Vec3::Z,
            astrocore_sprites::DirectionSet::Four,
        );
        commands.entity(e).insert(PlacementBillboard(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_with_defaults_and_rejects_unsafe_paths() {
        let m: RenderManifest = serde_json::from_str(
            r#"{"materials":{"ss14:wall":{"image":"a/b.png"}},"sprites":{"ss14:crowbar":{"image":"c.png"}}}"#,
        )
        .unwrap();
        assert_eq!(m.sprites["ss14:crowbar"].size, [0.5, 0.5]);
        assert!(
            serde_json::from_str::<RenderManifest>("{}")
                .unwrap()
                .materials
                .is_empty()
        );
        let base = Path::new("/srv/assets");
        assert!(safe_join(base, "a/b.png").is_ok());
        for bad in ["../x.png", "/etc/passwd", "", "a/../../b.png"] {
            assert!(safe_join(base, bad).is_err(), "{bad:?}");
        }
    }

    /// Минимальный корректный PNG 1×1.
    const PNG_1X1: [u8; 68] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0x0F, 0x04, 0x00, 0x09, 0xFB, 0x03, 0xFD, 0xFB, 0x5E, 0x6B, 0x2B, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn installing_loads_pngs_and_refuses_bad_files_and_ids() {
        let dir = std::env::temp_dir().join(format!("astro-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("w.png"), PNG_1X1).unwrap();
        std::fs::write(dir.join("broken.png"), b"not a png").unwrap();
        let mut images = Assets::<Image>::default();
        let ok: RenderManifest = serde_json::from_str(
            r#"{"materials":{"ss14:wall":{"image":"w.png"}},"sprites":{"ss14:pen":{"image":"w.png","size":[0.2,0.3]}}}"#,
        )
        .unwrap();
        let (t, s) = ok.install(&dir, &mut images).unwrap();
        assert_eq!(t.0.len(), 1);
        let pen = &s.0[&PrototypeId::new("ss14:pen").unwrap()];
        assert_eq!(pen.size, Vec2::new(0.2, 0.3));
        let bad = |json: &str| {
            serde_json::from_str::<RenderManifest>(json)
                .unwrap()
                .install(&dir, &mut Assets::<Image>::default())
                .is_err()
        };
        assert!(bad(r#"{"materials":{"ss14:wall":{"image":"broken.png"}}}"#));
        assert!(bad(
            r#"{"materials":{"ss14:wall":{"image":"missing.png"}}}"#
        ));
        assert!(bad(r#"{"materials":{"Плохо":{"image":"w.png"}}}"#));
        assert!(bad(
            r#"{"sprites":{"ss14:x":{"image":"w.png","size":[0,1]}}}"#
        ));
        assert!(bad(r#"{"materials":{"ss14:wall":{"image":"../w.png"}}}"#));
        let _ = std::fs::remove_dir_all(dir);
    }
}
