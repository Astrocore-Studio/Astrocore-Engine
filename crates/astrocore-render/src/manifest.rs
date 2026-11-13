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
    /// Как рисовать: стоячий билборд (по умолчанию) или лежащий на полу плашмя с толщиной.
    #[serde(default)]
    pub layout: SpriteLayout,
    /// Толщина лежащего спрайта, м (слои спрайта, уложенные стопкой); по умолчанию 3 см.
    #[serde(default = "default_thickness")]
    pub thickness: f32,
    /// Лист из `[столбцов, строк]` ячеек (направления и кадры анимации): берётся первая ячейка.
    #[serde(default)]
    pub grid: Option<[u32; 2]>,
    /// Кадров анимации на направление (для `box4`; ячейки листа идут направление за направлением).
    #[serde(default = "one_frame")]
    pub frames: u32,
    /// Ориентированный лежащий спрайт (конвейер, труба): поворот по `rot` карты, а не случайный.
    #[serde(default)]
    pub oriented: bool,
}

/// Вид размещения в мире.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpriteLayout {
    /// Вертикальный спрайт, повёрнутый к камере.
    #[default]
    Billboard,
    /// Спрайт лежит на полу плашмя и имеет толщину.
    Flat,
    /// Стоячий спрайт, выдавленный в объём: каждый непрозрачный пиксель — столбик глубиной
    /// `thickness`, спереди и сзади рисунок, по бокам цвет крайних пикселей. Повёрнут по `rot`.
    Voxel,
    /// Брус из четырёх видов RSI: лицо — «юг», спина — «север», бока — «восток» и «запад»
    /// (лист `grid` в порядке S, N, E, W; у анимации берётся первый кадр каждого направления).
    Box4,
}

fn one_frame() -> u32 {
    1
}

fn default_thickness() -> f32 {
    0.03
}

fn default_sprite_size() -> [f32; 2] {
    [0.5, 0.5]
}

/// Соответствие идентификаторов движка картинкам: клиент не знает о форматах игры, игровой
/// билд сам генерирует этот файл из своих прототипов и ассетов.
/// Лист состояния анимации двери: сетка ячеек, кадры идут по строкам слева направо.
#[derive(Debug, Clone, Deserialize)]
pub struct SheetEntry {
    pub image: String,
    pub cols: u32,
    pub rows: u32,
    /// Задержки кадров, секунды; для статичного состояния — один кадр (задержка не нужна).
    #[serde(default)]
    pub delays: Vec<f32>,
}

/// Спрайты двери по состояниям.
#[derive(Debug, Clone, Deserialize)]
pub struct DoorEntry {
    pub closed: SheetEntry,
    pub open: SheetEntry,
    pub opening: SheetEntry,
    pub closing: SheetEntry,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RenderManifest {
    /// Анимированные двери: идентификатор материала проёма → спрайты состояний.
    #[serde(default)]
    pub doors: BTreeMap<String, DoorEntry>,
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
    pub layout: SpriteLayout,
    pub thickness: f32,
    pub oriented: bool,
    /// Лист `[столбцов, строк]` и кадров на направление (для `box4`).
    pub grid: [u32; 2],
    pub frames: u32,
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
pub(crate) fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, ManifestError> {
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

pub(crate) fn load_png(path: &Path, repeat: bool) -> Result<Image, ManifestError> {
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
    ) -> Result<
        (
            MaterialTextures,
            PlacementSprites,
            crate::doors::DoorSprites,
        ),
        ManifestError,
    > {
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
            let mut img = load_png(&safe_join(base, &e.image)?, false)?;
            if let Some([cols, rows]) = e.grid
                && e.layout != SpriteLayout::Box4
            {
                img = first_cell(img, cols, rows);
            }
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
                    layout: e.layout,
                    thickness: e.thickness.clamp(0.001, 0.5),
                    oriented: e.oriented,
                    grid: e.grid.unwrap_or([1, 1]),
                    frames: e.frames.max(1),
                },
            );
        }
        let doors = crate::doors::install_doors(&self.doors, base, images)?;
        Ok((textures, sprites, doors))
    }
}

/// Первая ячейка листа (`cols × rows`): направление «юг», первый кадр анимации.
fn first_cell(image: Image, cols: u32, rows: u32) -> Image {
    let (w, h) = (image.width(), image.height());
    if (cols <= 1 && rows <= 1) || cols == 0 || rows == 0 || w % cols != 0 || h % rows != 0 {
        return image;
    }
    let Some(data) = image.data.as_ref() else {
        return image;
    };
    let (cw, ch) = ((w / cols) as usize, (h / rows) as usize);
    let stride = w as usize * 4;
    if data.len() < stride * h as usize {
        return image;
    }
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in 0..ch {
        out.extend_from_slice(&data[y * stride..y * stride + cw * 4]);
    }
    let mut cropped = Image::new(
        bevy::render::render_resource::Extent3d {
            width: cw as u32,
            height: ch as u32,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        out,
        image.texture_descriptor.format,
        RenderAssetUsages::default(),
    );
    cropped.sampler = image.sampler.clone();
    cropped
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
    images: Res<Assets<Image>>,
    existing: Query<(Entity, &PlacementBillboard)>,
    mut seen: Local<Option<(u64, usize)>>,
    mut voxel_meshes: Local<VoxelMeshes>,
) {
    let Some(rendered) = rendered else { return };
    let key = (rendered.world.revision().0, sprites.0.len());
    if *seen == Some(key) {
        return;
    }
    *seen = Some(key);
    let rot: HashMap<ObjectId, u8> = rendered
        .world
        .authored()
        .filter_map(|o| {
            let q = o.metadata.get("rot")?.parse::<u8>().ok()?;
            Some((o.id, q % 4))
        })
        .collect();
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
            let mut t = Transform::from_translation(feet);
            if matches!(sprite.layout, SpriteLayout::Voxel | SpriteLayout::Box4) {
                t.rotation = Quat::from_rotation_y(
                    f32::from(rot.get(&id).copied().unwrap_or(0)) * std::f32::consts::FRAC_PI_2,
                );
            }
            commands.entity(have[&id]).insert(t);
            continue;
        }
        if matches!(sprite.layout, SpriteLayout::Voxel | SpriteLayout::Box4)
            && let Some(mesh) = voxel_meshes.get(sprite, &images, &mut meshes)
        {
            let material = voxel_material(sprite, &mut materials);
            let quarters = rot.get(&id).copied().unwrap_or(0);
            let e = commands
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(material),
                    Transform::from_translation(feet).with_rotation(Quat::from_rotation_y(
                        f32::from(quarters) * std::f32::consts::FRAC_PI_2,
                    )),
                    PlacementBillboard(id),
                ))
                .id();
            let _ = e;
            continue;
        }
        // Лежащие предметы — настоящий объём в пару пикселей (`thickness`), лицом вверх; пока
        // картинка не загружена или слишком велика, ниже остаются слои-пластины.
        if sprite.layout == SpriteLayout::Flat
            && let Some(mesh) = voxel_meshes.get(sprite, &images, &mut meshes)
        {
            let material = voxel_material(sprite, &mut materials);
            let yaw = match sprite.oriented.then(|| rot.get(&id).copied().unwrap_or(0)) {
                Some(q) => f32::from(q) * std::f32::consts::FRAC_PI_2,
                None => (id.as_uuid().as_u128() % 360) as f32 * std::f32::consts::PI / 180.0,
            };
            let spin = Quat::from_rotation_y(yaw);
            // Модель стоит на «пластине» XY лицом к +Z: кладём лицом вверх, низ рисунка к −Z.
            let offset = Vec3::new(0.0, sprite.thickness * 0.5 + 0.004, sprite.size.y * 0.5);
            commands.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_translation(feet + spin * offset)
                    .with_rotation(spin * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                PlacementBillboard(id),
            ));
            continue;
        }
        if sprite.layout == SpriteLayout::Flat {
            let e = spawn_flat(
                &mut commands,
                &mut meshes,
                &mut materials,
                id,
                feet,
                sprite,
                if sprite.oriented {
                    Some(rot.get(&id).copied().unwrap_or(0))
                } else {
                    None
                },
            );
            commands.entity(e).insert(PlacementBillboard(id));
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

/// Сетки вокселей по картинке: одна на спрайт, а не на каждый предмет.
#[derive(Default)]
pub struct VoxelMeshes(HashMap<AssetId<Image>, Handle<Mesh>>);

impl VoxelMeshes {
    fn get(
        &mut self,
        sprite: &PlacementSprite,
        images: &Assets<Image>,
        meshes: &mut Assets<Mesh>,
    ) -> Option<Handle<Mesh>> {
        if let Some(h) = self.0.get(&sprite.image.id()) {
            return Some(h.clone());
        }
        let img = images.get(&sprite.image)?;
        let data = img.data.as_ref()?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let mesh = if sprite.layout == SpriteLayout::Box4 {
            box4_mesh(data, w, h, sprite.grid, sprite.frames, sprite.size)?
        } else {
            voxel_mesh(data, w, h, sprite.size, sprite.thickness)?
        };
        let handle = meshes.add(mesh);
        self.0.insert(sprite.image.id(), handle.clone());
        Some(handle)
    }
}

fn voxel_material(
    sprite: &PlacementSprite,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color_texture: Some(sprite.image.clone()),
        reflectance: 0.05,
        alpha_mode: if sprite.layout == SpriteLayout::Box4 {
            AlphaMode::Mask(0.5)
        } else {
            AlphaMode::Opaque
        },
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        ..default()
    })
}

/// Объёмная модель из RGBA-картинки: основание в начале координат, лицо к +Z, ширина `size.x`,
/// высота `size.y`, глубина `depth` (по половине в обе стороны от плоскости XY). Грани рисуются
/// только там, где соседний пиксель прозрачен; цвет грани — цвет её пикселя (UV в центр пикселя).
/// `None`, если в картинке нет непрозрачных пикселей или она слишком велика.
pub fn voxel_mesh(rgba: &[u8], w: usize, h: usize, size: Vec2, depth: f32) -> Option<Mesh> {
    if w == 0 || h == 0 || w > 256 || h > 256 || rgba.len() < w * h * 4 {
        return None;
    }
    let solid = |x: i64, y: i64| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && rgba[(y as usize * w + x as usize) * 4 + 3] >= 128
    };
    let (pw, ph) = (size.x / w as f32, size.y / h as f32);
    let hd = depth * 0.5;
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut nor: Vec<[f32; 3]> = Vec::new();
    let mut uv: Vec<[f32; 2]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut quad = |p: [[f32; 3]; 4], n: [f32; 3], t: [f32; 2]| {
        let base = pos.len() as u32;
        pos.extend(p);
        nor.extend([n; 4]);
        uv.extend([t; 4]);
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let mut any = false;
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if !solid(x, y) {
                continue;
            }
            any = true;
            let t = [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32];
            let x0 = -size.x * 0.5 + x as f32 * pw;
            let x1 = x0 + pw;
            // Строка 0 картинки — верх спрайта.
            let y1 = size.y - y as f32 * ph;
            let y0 = y1 - ph;
            // Лицо (+Z) и спина (−Z).
            quad(
                [[x0, y0, hd], [x1, y0, hd], [x1, y1, hd], [x0, y1, hd]],
                [0.0, 0.0, 1.0],
                t,
            );
            quad(
                [[x1, y0, -hd], [x0, y0, -hd], [x0, y1, -hd], [x1, y1, -hd]],
                [0.0, 0.0, -1.0],
                t,
            );
            if !solid(x - 1, y) {
                quad(
                    [[x0, y0, -hd], [x0, y0, hd], [x0, y1, hd], [x0, y1, -hd]],
                    [-1.0, 0.0, 0.0],
                    t,
                );
            }
            if !solid(x + 1, y) {
                quad(
                    [[x1, y0, hd], [x1, y0, -hd], [x1, y1, -hd], [x1, y1, hd]],
                    [1.0, 0.0, 0.0],
                    t,
                );
            }
            if !solid(x, y - 1) {
                quad(
                    [[x0, y1, hd], [x1, y1, hd], [x1, y1, -hd], [x0, y1, -hd]],
                    [0.0, 1.0, 0.0],
                    t,
                );
            }
            if !solid(x, y + 1) {
                quad(
                    [[x0, y0, -hd], [x1, y0, -hd], [x1, y0, hd], [x0, y0, hd]],
                    [0.0, -1.0, 0.0],
                    t,
                );
            }
        }
    }
    if !any {
        return None;
    }
    let mut mesh = Mesh::new(
        bevy::render::render_resource::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nor);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_indices(bevy::mesh::Indices::U32(idx));
    Some(mesh)
}

/// Объём из четырёх видов RSI (визуальная оболочка). Лист (`grid` = столбцов × строк) хранит ячейки
/// в порядке направлений S, N, E, W (у анимации — по `frames` ячеек на направление, берётся первая).
/// Воксель заполнен, если непрозрачны и «юг» (ширина × высота), и «восток» (глубина × высота): так
/// скошенный экран консоли вырезается по профилю. Цвет граней берётся из своего вида: лицо (+Z) — «юг»,
/// спина (−Z) — «север», −X — «восток» (собственная правая сторона объекта), +X — «запад», верх — из «юга».
/// Основание в начале координат, лицо к +Z, размер ячейки в метрах — `size`.
pub fn box4_mesh(
    rgba: &[u8],
    w: usize,
    h: usize,
    grid: [u32; 2],
    frames: u32,
    size: Vec2,
) -> Option<Mesh> {
    let (cols, rows) = (grid[0] as usize, grid[1] as usize);
    if cols == 0 || rows == 0 || w % cols != 0 || h % rows != 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let (cw, ch) = (w / cols, h / rows);
    let frames = frames.max(1) as usize;
    if cols * rows < 4 * frames || cw == 0 || ch == 0 || cw > 128 || ch > 128 {
        return None;
    }
    // Начало ячейки направления `d` на листе, пиксели.
    let origin = |d: usize| -> (usize, usize) {
        let cell = d * frames;
        ((cell % cols) * cw, (cell / cols) * ch)
    };
    let opaque = |d: usize, x: usize, y: usize| -> bool {
        if x >= cw || y >= ch {
            return false;
        }
        let (ox, oy) = origin(d);
        rgba[((oy + y) * w + ox + x) * 4 + 3] >= 128
    };
    // Рамка непрозрачных пикселей ячейки (в координатах ячейки): (x0, y0, x1, y1), правая/нижняя исключены.
    let bbox = |d: usize| -> Option<(usize, usize, usize, usize)> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..ch {
            for x in 0..cw {
                if opaque(d, x, y) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x1 > x0).then_some((x0, y0, x1, y1))
    };
    let south = bbox(0)?;
    let east = bbox(2).unwrap_or(south);
    let (nx, ny, nz) = (south.2 - south.0, south.3 - south.1, east.2 - east.0);
    let (pw, ph) = (size.x / cw as f32, size.y / ch as f32);
    // Воксель (x, y, z): x слева направо в «юге», y сверху вниз, z от спины (0) к лицу (nz − 1).
    let solid = |x: i64, y: i64, z: i64| -> bool {
        if x < 0 || y < 0 || z < 0 || x >= nx as i64 || y >= ny as i64 || z >= nz as i64 {
            return false;
        }
        let (xs, ys) = (south.0 + x as usize, south.1 + y as usize);
        let ze = east.0 + z as usize;
        opaque(0, xs, ys) && opaque(2, ze, ys)
    };
    // Центр пикселя ячейки `d` в координатах листа.
    let uv = |d: usize, x: usize, y: usize| -> [f32; 2] {
        let (ox, oy) = origin(d);
        [
            (ox + x.min(cw - 1)) as f32 / w as f32 + 0.5 / w as f32,
            (oy + y.min(ch - 1)) as f32 / h as f32 + 0.5 / h as f32,
        ]
    };
    let x_off = -(nx as f32) * pw * 0.5;
    let z_off = -(nz as f32) * pw * 0.5;
    let top_y = ny as f32 * ph;
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut nor: Vec<[f32; 3]> = Vec::new();
    let mut tex: Vec<[f32; 2]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut quad = |p: [[f32; 3]; 4], n: [f32; 3], t: [f32; 2]| {
        let base = pos.len() as u32;
        pos.extend(p);
        nor.extend([n; 4]);
        tex.extend([t; 4]);
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let mut any = false;
    for y in 0..ny as i64 {
        for x in 0..nx as i64 {
            for z in 0..nz as i64 {
                if !solid(x, y, z) {
                    continue;
                }
                any = true;
                let (x0, x1) = (x_off + x as f32 * pw, x_off + (x + 1) as f32 * pw);
                let (y1, y0) = (top_y - y as f32 * ph, top_y - (y + 1) as f32 * ph);
                let (z0, z1) = (z_off + z as f32 * pw, z_off + (z + 1) as f32 * pw);
                let (xs, ys) = (south.0 + x as usize, south.1 + y as usize);
                if !solid(x, y, z + 1) {
                    quad(
                        [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
                        [0.0, 0.0, 1.0],
                        uv(0, xs, ys),
                    );
                }
                if !solid(x, y, z - 1) {
                    // Спина: зеркало по X в ячейке «севера».
                    let xn = south.0 + (nx - 1 - x as usize);
                    quad(
                        [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
                        [0.0, 0.0, -1.0],
                        uv(1, xn, ys),
                    );
                }
                if !solid(x - 1, y, z) {
                    // −X: «восток», слева направо от спины к лицу.
                    quad(
                        [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
                        [-1.0, 0.0, 0.0],
                        uv(2, east.0 + z as usize, ys),
                    );
                }
                if !solid(x + 1, y, z) {
                    // +X: «запад», слева направо от лица к спине.
                    let zw = east.0 + (nz - 1 - z as usize);
                    quad(
                        [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
                        [1.0, 0.0, 0.0],
                        uv(3, zw, ys),
                    );
                }
                if !solid(x, y - 1, z) {
                    quad(
                        [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
                        [0.0, 1.0, 0.0],
                        uv(0, xs, ys),
                    );
                }
            }
        }
    }
    if !any {
        return None;
    }
    let mut mesh = Mesh::new(
        bevy::render::render_resource::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nor);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, tex);
    mesh.insert_indices(bevy::mesh::Indices::U32(idx));
    Some(mesh)
}

/// Сколько слоёв в стопке лежащего спрайта.
const FLAT_LAYERS: usize = 6;

/// Лежащий спрайт: стопка одинаковых слоёв с вырезом по прозрачности. Сверху виден рисунок, сбоку —
/// толщина; нижние слои темнее. Поворот вокруг вертикали берётся из идентификатора объекта, чтобы
/// лежащие вещи не стояли строем.
fn spawn_flat(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    id: ObjectId,
    feet: Vec3,
    sprite: &PlacementSprite,
    rot: Option<u8>,
) -> Entity {
    let quad = meshes.add(Rectangle::new(sprite.size.x, sprite.size.y));
    // Повёрнутые на карте вещи (конвейеры, трубы) лежат по `rot`, остальные — как попало.
    let yaw = match rot {
        Some(q) => f32::from(q) * std::f32::consts::FRAC_PI_2,
        None => (id.as_uuid().as_u128() % 360) as f32 * std::f32::consts::PI / 180.0,
    };
    let step = sprite.thickness / FLAT_LAYERS as f32;
    commands
        .spawn((
            Transform::from_translation(feet + Vec3::Y * 0.004)
                .with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::default(),
        ))
        .with_children(|p| {
            for i in 0..FLAT_LAYERS {
                let shade = 0.55 + 0.45 * i as f32 / (FLAT_LAYERS - 1) as f32;
                let material = materials.add(StandardMaterial {
                    base_color: Color::srgb(shade, shade, shade),
                    base_color_texture: Some(sprite.image.clone()),
                    unlit: true,
                    alpha_mode: AlphaMode::Mask(0.5),
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                });
                p.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(material),
                    Transform::from_xyz(0.0, step * i as f32, 0.0)
                        .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                ));
            }
        })
        .id()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_box_is_carved_by_the_side_profile() {
        // Лист 2×2 по 4×4: «юг» — полный квадрат; «восток» — клин: верхняя строка 1 пиксель, нижние 4.
        let (w, h) = (8usize, 8usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut put = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[200, 100, 50, 255]);
        };
        for y in 0..4 {
            for x in 0..4 {
                put(x, y); // юг
                put(4 + x, y); // север
            }
        }
        for y in 0..4 {
            // Восток — ячейка (0, 1): клин, чем выше строка, тем уже.
            for x in 0..(1 + y) {
                put(x, 4 + y);
            }
        }
        let mesh = super::box4_mesh(&rgba, w, h, [2, 2], 1, Vec2::new(1.0, 1.0)).expect("объём");
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| a.as_float3())
            .unwrap()
            .to_vec();
        // Глубина — 4 пикселя по 0,25 м: от −0,5 до 0,5 по Z.
        let (min_z, max_z) = positions
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[2]), b.max(p[2])));
        assert!(
            (min_z + 0.5).abs() < 1e-4 && (max_z - 0.5).abs() < 1e-4,
            "{min_z} {max_z}"
        );
        // У верхней строки клина один воксель в глубину: грани лица уходят к спине.
        let top_front = positions
            .iter()
            .filter(|p| (p[1] - 1.0).abs() < 1e-4)
            .map(|p| p[2])
            .fold(f32::MIN, f32::max);
        assert!(top_front < 0.0, "верх клина узкий: {top_front}");
    }

    #[test]
    fn the_first_cell_of_a_sheet_is_cut_out() {
        // Лист 2×1: левая ячейка красная, правая синяя.
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
        }
        let img = Image::new(
            bevy::render::render_resource::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            data.iter()
                .copied()
                .collect::<Vec<u8>>()
                .chunks(8)
                .flat_map(|c| c.to_vec())
                .collect(),
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        let cut = super::first_cell(img, 2, 1);
        assert_eq!((cut.width(), cut.height()), (1, 2));
        assert_eq!(&cut.data.as_ref().unwrap()[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn a_sprite_becomes_a_voxel_slab_with_side_faces_only_on_the_outline() {
        // 2×2 картинка: три непрозрачных пикселя, правый нижний прозрачен.
        let mut rgba = vec![0u8; 2 * 2 * 4];
        for (i, a) in [255u8, 255, 255, 0].iter().enumerate() {
            rgba[i * 4] = 200;
            rgba[i * 4 + 3] = *a;
        }
        let mesh = super::voxel_mesh(&rgba, 2, 2, Vec2::new(1.0, 1.0), 0.2).expect("сетка");
        // Лицо и спина у 3 пикселей (6 граней) + боковые: у верхнего левого — лево, верх;
        // у верхнего правого — право, верх, низ; у нижнего левого — лево, низ, право.
        let n = mesh.count_vertices();
        assert_eq!(n, 4 * (6 + 2 + 3 + 3));
        assert!(
            super::voxel_mesh(&[0; 16], 2, 2, Vec2::ONE, 0.2).is_none(),
            "пустая картинка"
        );
    }

    use super::*;

    #[test]
    fn flat_layout_and_thickness_parse() {
        let m: RenderManifest = serde_json::from_str(
            r#"{"sprites":{"ss14:pen":{"image":"w.png","layout":"flat","thickness":0.02},"ss14:can":{"image":"c.png"}}}"#,
        )
        .unwrap();
        assert_eq!(m.sprites["ss14:pen"].layout, SpriteLayout::Flat);
        assert!((m.sprites["ss14:pen"].thickness - 0.02).abs() < 1e-6);
        assert_eq!(m.sprites["ss14:can"].layout, SpriteLayout::Billboard);
        assert!((m.sprites["ss14:can"].thickness - 0.03).abs() < 1e-6);
    }

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
        let (t, s, _) = ok.install(&dir, &mut images).unwrap();
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
