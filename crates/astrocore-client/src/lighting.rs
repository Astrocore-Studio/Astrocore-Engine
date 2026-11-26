//! Свет и поле зрения клиента: карта освещения считается на процессоре с шагом в долю
//! тайла и умножается на нарисованный мир. Образец — освещение и FOV `Clyde` Robust.
//!
//! Каждый источник хранит готовое пятно света с тенями; оно пересчитывается, только когда
//! источник сдвинулся или рядом изменились преграды. Кадр складывает пятна видимых
//! источников и закрывает чёрным всё, что не видно из глаза.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use astrocore_light::{OccluderGrid, PointLight, collect_occluders};
use astrocore_map::{Transform as GameTransform, TransformWorldExt, WorldTransform};
use astrocore_net::{ClientNet, Detached};
use bevy::asset::{AssetPath, RenderAssetUsages, embedded_asset, embedded_path};
use bevy::image::ImageSampler;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d,
    RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{
    AlphaMode2d, Material2d, Material2dKey, Material2dPipeline, Material2dPlugin,
};
use bevy::window::PrimaryWindow;

use crate::Game;
use crate::view::{Eye, VIEW_TILES};

/// Точек карты освещения на сторону тайла.
pub const TEXELS_PER_TILE: i32 = 8;

/// Наибольшая дальность источника, которую учитывает поиск источников и преград, тайлы.
const MAX_LIGHT_RADIUS: f32 = 16.0;

/// Сколько пятен света считается за кадр: остальные источники загорятся в следующих
/// кадрах, зато вход в освещённый зал не даёт рывка.
const NEW_LIGHTS_PER_FRAME: usize = 8;

/// Глубина карты освещения: поверх мира, под интерфейсом.
const OVERLAY_DEPTH: f32 = 950.0;

/// Настройки света клиента.
#[derive(Resource, Debug, Clone, Copy)]
pub struct Lighting {
    /// Свет и тени включены; иначе мир рисуется полностью освещённым.
    pub enabled: bool,
    /// Невидимое из глаза закрыто чёрным (`DrawFov` глаза в Robust).
    pub fov: bool,
    /// Свет там, куда не достают источники (`AmbientLightColor` карты).
    pub ambient: astrocore_math::Color,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            enabled: true,
            fov: true,
            ambient: astrocore_math::Color::BLACK,
        }
    }
}

/// Материал карты освещения: её цвет умножается на нарисованный под ней мир.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct LightMaterial {
    #[texture(0)]
    #[sampler(1)]
    texture: Handle<Image>,
}

impl Material2d for LightMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(
            AssetPath::from_path_buf(embedded_path!("light.wesl")).with_source("embedded"),
        )
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    fn specialize(
        _: &Material2dPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        _: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Итог = мир × свет; прозрачность мира не меняется.
        let multiply = BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::Dst,
                dst_factor: BlendFactor::Zero,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::Zero,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        };
        if let Some(fragment) = &mut descriptor.fragment {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(multiply);
            }
        }
        Ok(())
    }
}

/// Пятно света одного источника: доля яркости по точкам карты освещения, с тенями.
struct Kernel {
    key: u64,
    /// Левая нижняя точка пятна на общей сетке карты освещения.
    origin: (i32, i32),
    size: i32,
    values: Vec<u8>,
}

#[derive(Component)]
pub(crate) struct LightOverlay;

/// Состояние расчёта света.
#[derive(Resource, Default)]
pub(crate) struct LightState {
    kernels: HashMap<bevy_ecs::entity::Entity, Kernel>,
    /// Ключ и маска поля зрения последнего кадра.
    fov: Option<(u64, Vec<bool>)>,
    overlay: Option<(Entity, Handle<Image>, Handle<LightMaterial>)>,
}

/// Источник света кадра.
struct Lamp {
    entity: bevy_ecs::entity::Entity,
    light: PointLight,
    position: astrocore_math::Vec2,
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Перевод точки мира в координаты грида глаза; без грида координаты совпадают.
fn to_grid(grid: &Option<WorldTransform>, point: astrocore_math::Vec2) -> astrocore_math::Vec2 {
    grid.as_ref().map_or(point, |grid| grid.to_local(point))
}

fn texel_center(x: i32, y: i32) -> astrocore_math::Vec2 {
    let scale = TEXELS_PER_TILE as f32;
    astrocore_math::Vec2::new((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale)
}

fn compute_kernel(
    key: u64,
    lamp: &Lamp,
    occluders: &OccluderGrid,
    grid: &Option<WorldTransform>,
) -> Kernel {
    let scale = TEXELS_PER_TILE as f32;
    let reach = (lamp.light.radius * scale).ceil() as i32;
    let origin = (
        (lamp.position.x * scale).floor() as i32 - reach,
        (lamp.position.y * scale).floor() as i32 - reach,
    );
    let size = reach * 2 + 1;
    let source = to_grid(grid, lamp.position);
    let mut values = vec![0u8; (size * size) as usize];
    for j in 0..size {
        for i in 0..size {
            let point = texel_center(origin.0 + i, origin.1 + j);
            let strength = lamp.light.attenuation(point.distance(lamp.position));
            if strength <= 0.0 {
                continue;
            }
            if lamp.light.cast_shadows && !occluders.visible(source, to_grid(grid, point)) {
                continue;
            }
            values[(j * size + i) as usize] = (strength * 255.0).round() as u8;
        }
    }
    Kernel {
        key,
        origin,
        size,
        values,
    }
}

/// Собирает карту освещения кадра и ставит её поверх мира.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn update_lighting(
    mut commands: Commands,
    mut game: ResMut<Game>,
    eye: Res<Eye>,
    lighting: Res<Lighting>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    camera: Single<&Transform, (With<Camera2d>, Without<LightOverlay>)>,
    mut state: ResMut<LightState>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<LightMaterial>>,
    mut overlays: Query<(&mut Transform, &mut Visibility), With<LightOverlay>>,
) {
    let world = &mut game.0;
    let player = world
        .get_resource::<ClientNet>()
        .and_then(|net| net.attached);
    let (true, Some(map), Some(player)) = (lighting.enabled, eye.map, player) else {
        for (_, mut visibility) in &mut overlays {
            *visibility = Visibility::Hidden;
        }
        return;
    };

    // Прямоугольник карты освещения: окно камеры с запасом в тайл, по общей сетке точек.
    let scale = TEXELS_PER_TILE as f32;
    let aspect = window
        .filter(|window| window.height() > 0.0)
        .map_or(16.0 / 9.0, |window| window.width() / window.height());
    let half = Vec2::new(VIEW_TILES * aspect / 2.0 + 1.0, VIEW_TILES / 2.0 + 1.0);
    let center = camera.translation.truncate();
    let min = ((center - half) * scale).floor().as_ivec2();
    let size = ((half * 2.0) * scale).ceil().as_ivec2() + IVec2::ONE;
    let (width, height) = (size.x as usize, size.y as usize);

    // Преграды грида, на котором стоит игрок.
    let grid_entity = world.get::<GameTransform>(player).and_then(|t| t.grid());
    let grid = grid_entity.and_then(|grid| world.world_transform(grid));
    let occluders = match grid_entity {
        Some(grid_entity) => {
            let reach = half + Vec2::splat(MAX_LIGHT_RADIUS);
            let corners = [
                Vec2::new(-reach.x, -reach.y),
                Vec2::new(reach.x, -reach.y),
                Vec2::new(-reach.x, reach.y),
                Vec2::new(reach.x, reach.y),
            ]
            .map(|corner| {
                let point = center + corner;
                to_grid(&grid, astrocore_math::Vec2::new(point.x, point.y))
            });
            let low = corners.iter().fold(corners[0], |a, b| a.min(*b)).floor();
            let high = corners.iter().fold(corners[0], |a, b| a.max(*b)).ceil();
            collect_occluders(
                world,
                grid_entity,
                eye.level,
                (low.x as i32, low.y as i32),
                ((high.x - low.x) as i32 + 1, (high.y - low.y) as i32 + 1),
            )
        }
        None => OccluderGrid::new(0, 0, 0, 0),
    };
    let grid_key = grid.map(|g| {
        (
            g.position.x.to_bits(),
            g.position.y.to_bits(),
            g.rotation.0.to_bits(),
        )
    });

    // Источники света рядом с окном.
    let mut lamps: Vec<Lamp> = Vec::new();
    let mut sources = world.query_filtered::<(
        bevy_ecs::entity::Entity,
        &PointLight,
        &GameTransform,
    ), Without<Detached>>();
    for (entity, light, transform) in sources.iter(world) {
        if !light.enabled
            || light.radius <= 0.0
            || light.energy <= 0.0
            || transform.map() != Some(map)
        {
            continue;
        }
        let Some(placement) = world.world_transform(entity) else {
            continue;
        };
        let position = placement.to_world(light.offset);
        let radius = light.radius.min(MAX_LIGHT_RADIUS);
        let away = (Vec2::new(position.x, position.y) - center).abs();
        if placement.level != eye.level || away.x > half.x + radius || away.y > half.y + radius {
            continue;
        }
        lamps.push(Lamp {
            entity,
            light: PointLight {
                radius,
                ..light.clone()
            },
            position,
        });
    }

    // Пятна света: готовые берутся из памяти, изменившиеся пересчитываются.
    let mut budget = NEW_LIGHTS_PER_FRAME;
    state
        .kernels
        .retain(|entity, _| lamps.iter().any(|lamp| lamp.entity == *entity));
    for lamp in &lamps {
        let local = to_grid(&grid, lamp.position);
        let key = hash_of((
            // Сдвиг источника заметен с половины точки карты освещения.
            (lamp.position.x * scale * 2.0).round() as i32,
            (lamp.position.y * scale * 2.0).round() as i32,
            lamp.light.radius.to_bits(),
            lamp.light.falloff.to_bits(),
            lamp.light.curve_factor.to_bits(),
            lamp.light.cast_shadows,
            grid_key,
            lamp.light.cast_shadows.then(|| {
                occluders.window(
                    local.x.floor() as i32,
                    local.y.floor() as i32,
                    lamp.light.radius.ceil() as i32,
                )
            }),
        ));
        if state
            .kernels
            .get(&lamp.entity)
            .is_some_and(|kernel| kernel.key == key)
        {
            continue;
        }
        if budget == 0 {
            // Устаревшее пятно лучше убрать, чем показывать свет сквозь новую стену.
            state.kernels.remove(&lamp.entity);
            continue;
        }
        budget -= 1;
        state
            .kernels
            .insert(lamp.entity, compute_kernel(key, lamp, &occluders, &grid));
    }

    // Сумма света по точкам.
    let ambient =
        bevy::color::Color::srgb(lighting.ambient.r, lighting.ambient.g, lighting.ambient.b)
            .to_linear();
    let mut buffer = vec![[ambient.red, ambient.green, ambient.blue]; width * height];
    for lamp in &lamps {
        let Some(kernel) = state.kernels.get(&lamp.entity) else {
            continue;
        };
        let color =
            bevy::color::Color::srgb(lamp.light.color.r, lamp.light.color.g, lamp.light.color.b)
                .to_linear();
        let tint = [color.red, color.green, color.blue].map(|c| c * lamp.light.energy / 255.0);
        let x_range =
            kernel.origin.0.max(min.x)..(kernel.origin.0 + kernel.size).min(min.x + size.x);
        let y_range =
            kernel.origin.1.max(min.y)..(kernel.origin.1 + kernel.size).min(min.y + size.y);
        for y in y_range {
            let kernel_row = ((y - kernel.origin.1) * kernel.size - kernel.origin.0) as isize;
            let buffer_row = ((y - min.y) * size.x - min.x) as isize;
            for x in x_range.clone() {
                let value = kernel.values[(kernel_row + x as isize) as usize];
                if value != 0 {
                    let texel = &mut buffer[(buffer_row + x as isize) as usize];
                    let value = f32::from(value);
                    texel[0] += tint[0] * value;
                    texel[1] += tint[1] * value;
                    texel[2] += tint[2] * value;
                }
            }
        }
    }

    // Поле зрения: пересчитывается, когда глаз перешёл на другую точку или изменились
    // преграды.
    if lighting.fov {
        let eye_point = astrocore_math::Vec2::new(eye.position.x, eye.position.y);
        let key = hash_of((
            (eye_point.x * scale).floor() as i32,
            (eye_point.y * scale).floor() as i32,
            min,
            size,
            grid_key,
            &occluders,
        ));
        if state.fov.as_ref().is_none_or(|(old, _)| *old != key) {
            let from = to_grid(&grid, eye_point);
            let mut mask = Vec::with_capacity(width * height);
            for y in 0..size.y {
                for x in 0..size.x {
                    let point = texel_center(min.x + x, min.y + y);
                    mask.push(occluders.visible(from, to_grid(&grid, point)));
                }
            }
            state.fov = Some((key, mask));
        }
        if let Some((_, mask)) = &state.fov {
            for (texel, visible) in buffer.iter_mut().zip(mask) {
                if !visible {
                    *texel = [0.0; 3];
                }
            }
        }
    }

    // Картинка: строка 0 — верх.
    let mut data = vec![255u8; width * height * 4];
    for y in 0..height {
        let row = (height - 1 - y) * width * 4;
        for x in 0..width {
            let texel = buffer[y * width + x];
            for channel in 0..3 {
                data[row + x * 4 + channel] =
                    (texel[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::linear();

    let target = Transform::from_xyz(
        (min.x as f32 + size.x as f32 / 2.0) / scale,
        (min.y as f32 + size.y as f32 / 2.0) / scale,
        OVERLAY_DEPTH,
    )
    .with_scale(Vec3::new(size.x as f32 / scale, size.y as f32 / scale, 1.0));
    match &state.overlay {
        Some((overlay, image_handle, material)) => {
            let _ = images.insert(image_handle, image);
            // Материал помечается изменённым, чтобы отрисовка взяла новую картинку.
            materials.get_mut(material);
            if let Ok((mut transform, mut visibility)) = overlays.get_mut(*overlay) {
                *transform = target;
                *visibility = Visibility::Inherited;
            }
        }
        None => {
            let image_handle = images.add(image);
            let material = materials.add(LightMaterial {
                texture: image_handle.clone(),
            });
            let overlay = commands
                .spawn((
                    Mesh2d(meshes.add(Rectangle::new(1.0, 1.0))),
                    MeshMaterial2d(material.clone()),
                    target,
                    LightOverlay,
                ))
                .id();
            state.overlay = Some((overlay, image_handle, material));
        }
    }
}

/// Подключает свет к приложению клиента.
pub(crate) fn build(app: &mut App) {
    embedded_asset!(app, "light.wesl");
    app.add_plugins(Material2dPlugin::<LightMaterial>::default())
        .init_resource::<Lighting>()
        .init_resource::<LightState>();
}
