//! Направленные спрайты в 3D-сцене.
//!
//! Спрайт — вертикальный квадрат, повёрнутый к камере вокруг оси Y (ноги остаются
//! на земле). Из листа выбирается кадр по направлению объекта относительно камеры.
//! Порядок столбцов листа — как в RSI: S, N, E, W (для 8 направлений далее SE, NE, SW, NW).

use astrocore_math::glam::Vec3 as MathVec3;
use astrocore_sprites::{DirectionSet, select_direction};
use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Лист направленных кадров: `columns` кадров в ряд, один ряд.
#[derive(Clone)]
pub struct SpriteSheet {
    pub image: Handle<Image>,
    pub columns: u32,
}

#[derive(Component, Debug, Clone)]
pub struct DirectionalSprite {
    /// Куда смотрит объект (горизонтальная проекция).
    pub facing: Vec3,
    pub set: DirectionSet,
    /// Размер в метрах: ширина × высота.
    pub size: Vec2,
    columns: u32,
}

/// Позиция ног — `Transform.translation`; квадрат смещён вверх на половину высоты дочерней сущностью.
#[derive(Component)]
pub struct BillboardQuad;

pub struct BillboardPlugin;

impl Plugin for BillboardPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            update_billboards.before(TransformSystems::Propagate),
        );
    }
}

/// Вертикальный квадрат с основанием в начале координат, лицом к +Z.
fn quad_mesh(width: f32, height: f32) -> Mesh {
    let (hw, h) = (width * 0.5, height);
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[-hw, 0.0, 0.0], [hw, 0.0, 0.0], [hw, h, 0.0], [-hw, h, 0.0]],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
    );
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

/// Создать спрайт. У каждого спрайта свой материал (своё смещение UV для выбранного кадра).
#[allow(clippy::too_many_arguments)]
pub fn spawn_billboard(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sheet: &SpriteSheet,
    feet: Vec3,
    size: Vec2,
    facing: Vec3,
    set: DirectionSet,
) -> Entity {
    let material = materials.add(StandardMaterial {
        base_color_texture: Some(sheet.image.clone()),
        unlit: true,
        alpha_mode: AlphaMode::Mask(0.5),
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands
        .spawn((
            Transform::from_translation(feet),
            Visibility::default(),
            DirectionalSprite {
                facing,
                set,
                size,
                columns: sheet.columns,
            },
        ))
        .with_children(|p| {
            p.spawn((
                Mesh3d(meshes.add(quad_mesh(size.x, size.y))),
                MeshMaterial3d(material),
                BillboardQuad,
            ));
        })
        .id()
}

fn update_billboards(
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut sprites: Query<
        (
            &GlobalTransform,
            &mut Transform,
            &DirectionalSprite,
            &Children,
        ),
        Without<Camera3d>,
    >,
    quads: Query<&MeshMaterial3d<StandardMaterial>, With<BillboardQuad>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(cam) = camera.single() else { return };
    let cam_pos = cam.translation();
    for (global, mut transform, sprite, children) in &mut sprites {
        let pos = global.translation();
        // Поворот вокруг Y: квадрат смотрит (+Z) на камеру.
        let to_cam = cam_pos - pos;
        transform.rotation = Quat::from_rotation_y(to_cam.x.atan2(to_cam.z));
        let dir = select_direction(
            MathVec3::new(sprite.facing.x, sprite.facing.y, sprite.facing.z),
            MathVec3::new(pos.x, pos.y, pos.z),
            MathVec3::new(cam_pos.x, cam_pos.y, cam_pos.z),
            sprite.set,
        );
        let column = (dir.index() as u32).min(sprite.columns.saturating_sub(1));
        for &child in children {
            if let Ok(handle) = quads.get(child)
                && let Some(mut mat) = materials.get_mut(&handle.0)
            {
                let scale = 1.0 / sprite.columns as f32;
                mat.uv_transform = bevy::math::Affine2::from_scale_angle_translation(
                    Vec2::new(scale, 1.0),
                    0.0,
                    Vec2::new(column as f32 * scale, 0.0),
                );
            }
        }
    }
}

/// Демонстрационный лист 4×1 (S, N, E, W): человечек, у которого на «лице» стороны
/// разный рисунок. Только для проверки рендера — настоящие спрайты приходят из билда (RSI).
pub fn demo_sprite_sheet(images: &mut Assets<Image>) -> SpriteSheet {
    const W: u32 = 16;
    const H: u32 = 24;
    const COLS: u32 = 4;
    let mut data = vec![0u8; (W * H * COLS * 4) as usize];
    let mut put = |col: u32, x: u32, y: u32, rgba: [u8; 4]| {
        if x < W && y < H {
            let i = (((y * W * COLS) + col * W + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&rgba);
        }
    };
    let body = [
        [230, 80, 80, 255],
        [80, 120, 230, 255],
        [80, 200, 110, 255],
        [230, 200, 70, 255],
    ];
    for col in 0..COLS {
        for y in 4..24 {
            for x in 4..12 {
                put(col, x, y, body[col as usize]);
            }
        }
        for y in 0..4 {
            for x in 5..11 {
                put(col, x, y, [250, 220, 180, 255]);
            }
        }
        // «Лицо»: S — два глаза, N — затылок (тёмный), E — глаз справа, W — глаз слева.
        let eye = [20, 20, 20, 255];
        match col {
            0 => {
                put(col, 6, 1, eye);
                put(col, 9, 1, eye);
            }
            1 => {
                for x in 5..11 {
                    put(col, x, 1, [60, 40, 30, 255]);
                    put(col, x, 0, [60, 40, 30, 255]);
                }
            }
            2 => put(col, 9, 1, eye),
            _ => put(col, 6, 1, eye),
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: W * COLS,
            height: H,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    SpriteSheet {
        image: images.add(image),
        columns: COLS,
    }
}
