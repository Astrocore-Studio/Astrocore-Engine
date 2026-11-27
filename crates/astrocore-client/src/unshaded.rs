//! Светящиеся слои спрайтов (`shader: unshaded`, ADR-0016): не затемняются освещением,
//! поле зрения их скрывает. Образец — шейдер `unshaded` и наложение света и поля зрения
//! `Clyde` Robust.
//!
//! Камера маски рисует в картинку размером с окно двойников слоёв: светящиеся — своим цветом,
//! обычные поверх них — чёрным. Прямоугольник поверх карты освещения прибавляет
//! `маска × видимость × (1 − свет)`: светящийся пиксель получает свой цвет целиком.

use bevy::asset::{AssetPath, embedded_asset, embedded_path};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{ClearColorConfig, RenderTarget};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    SpecializedMeshPipelineError, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{
    AlphaMode2d, Material2d, Material2dKey, Material2dPipeline, Material2dPlugin,
};
use bevy::window::PrimaryWindow;

use crate::view::{WorldCamera, world_projection};

/// Слой отрисовки двойников для маски светящегося.
pub const UNSHADED_LAYER: usize = 1;

/// Камера маски светящегося.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct UnshadedCamera;

/// Картинка маски: её рисует камера маски, читает прямоугольник сложения.
#[derive(Resource, Debug, Clone)]
pub(crate) struct UnshadedTarget {
    pub image: Handle<Image>,
    pub size: UVec2,
}

/// Материал сложения: карта освещения (в прозрачности — видимость) и маска светящегося.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct UnshadedMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub(crate) light: Handle<Image>,
    #[texture(2)]
    #[sampler(3)]
    pub(crate) mask: Handle<Image>,
    /// Размер окна в точках: по нему точка экрана переводится в координаты маски.
    #[uniform(4)]
    pub(crate) screen: Vec4,
}

impl Material2d for UnshadedMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(
            AssetPath::from_path_buf(embedded_path!("unshaded.wesl")).with_source("embedded"),
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
        // Итог = мир + цвет материала; прозрачность мира не меняется.
        let add = BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
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
                target.blend = Some(add);
            }
        }
        Ok(())
    }
}

fn target_image(size: UVec2) -> Image {
    Image::new_target_texture(
        size.x.max(1),
        size.y.max(1),
        TextureFormat::Rgba8UnormSrgb,
        None,
    )
}

pub(crate) fn spawn_camera(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let size = UVec2::ONE;
    let image = images.add(target_image(size));
    commands.spawn((
        UnshadedCamera,
        Camera2d,
        Camera {
            // Раньше камеры мира: прямоугольник сложения читает готовую маску.
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(image.clone().into()),
        world_projection(),
        RenderLayers::layer(UNSHADED_LAYER),
    ));
    commands.insert_resource(UnshadedTarget { image, size });
}

/// Камера маски повторяет камеру мира; картинка маски — размером с окно.
#[allow(clippy::type_complexity)]
pub(crate) fn follow_world_camera(
    world_camera: Single<(&Transform, &Projection), (With<WorldCamera>, Without<UnshadedCamera>)>,
    mut camera: Single<(&mut Transform, &mut Projection), With<UnshadedCamera>>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    mut target: ResMut<UnshadedTarget>,
    mut images: ResMut<Assets<Image>>,
) {
    let (world_transform, world_projection) = *world_camera;
    let (transform, projection) = &mut *camera;
    if **transform != *world_transform {
        **transform = *world_transform;
    }
    // Зум тоже повторяется: маска совпадает с экраном.
    if let (Projection::Orthographic(own), Projection::Orthographic(world)) =
        (&mut **projection, world_projection)
        && own.scale != world.scale
    {
        own.scale = world.scale;
    }
    let Some(window) = window else {
        return;
    };
    let size = window.physical_size().max(UVec2::ONE);
    if size != target.size {
        let _ = images.insert(&target.image, target_image(size));
        target.size = size;
    }
}

pub(crate) fn build(app: &mut App) {
    embedded_asset!(app, "unshaded.wesl");
    app.add_plugins(Material2dPlugin::<UnshadedMaterial>::default())
        .add_systems(Startup, spawn_camera);
}
