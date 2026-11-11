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

/// Слой спрайта (экипировка, наложенные кадры): свой лист, тот же поворот и направление, что у
/// родителя. Порядок слоёв задаётся смещением к камере, поэтому глубина разрешается без сортировки.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteLayer {
    pub order: u8,
    columns: u32,
}

/// Смещение слоя к камере на единицу порядка, м.
pub const LAYER_DEPTH_STEP: f32 = 0.004;

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

/// Добавить слой к спрайту, созданному `spawn_billboard`. Слой выбирает кадр по тому же
/// направлению, что и основа; лист слоя должен иметь столько же кадров-направлений.
pub fn spawn_sprite_layer(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    parent: Entity,
    sheet: &SpriteSheet,
    size: Vec2,
    order: u8,
) -> Entity {
    let material = materials.add(StandardMaterial {
        base_color_texture: Some(sheet.image.clone()),
        unlit: true,
        alpha_mode: AlphaMode::Mask(0.5),
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let layer = commands
        .spawn((
            Mesh3d(meshes.add(quad_mesh(size.x, size.y))),
            MeshMaterial3d(material),
            Transform::from_xyz(0.0, 0.0, LAYER_DEPTH_STEP * f32::from(order)),
            SpriteLayer {
                order,
                columns: sheet.columns,
            },
        ))
        .id();
    commands.entity(parent).add_child(layer);
    layer
}

type QuadFilter = Or<(With<BillboardQuad>, With<SpriteLayer>)>;
type QuadData = (
    &'static MeshMaterial3d<StandardMaterial>,
    Option<&'static SpriteLayer>,
);

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
    quads: Query<QuadData, QuadFilter>,
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
        for &child in children {
            if let Ok((handle, layer)) = quads.get(child)
                && let Some(mut mat) = materials.get_mut(&handle.0)
            {
                let columns = layer.map_or(sprite.columns, |l| l.columns);
                let column = (dir.index() as u32).min(columns.saturating_sub(1));
                let scale = 1.0 / columns as f32;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_follow_the_base_frame_and_stack_toward_the_camera() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin, BillboardPlugin))
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<Assets<StandardMaterial>>();
        app.world_mut()
            .spawn((Camera3d::default(), Transform::from_xyz(10.0, 1.0, 0.0)));
        let sheet = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            demo_sprite_sheet(&mut images)
        };
        let (base, l1, l2) = {
            let world = app.world_mut();
            let mut state = bevy::ecs::system::SystemState::<(
                Commands,
                ResMut<Assets<Mesh>>,
                ResMut<Assets<StandardMaterial>>,
            )>::new(world);
            let (mut commands, mut meshes, mut materials) = state.get_mut(world).unwrap();
            let base = spawn_billboard(
                &mut commands,
                &mut meshes,
                &mut materials,
                &sheet,
                Vec3::ZERO,
                Vec2::new(0.9, 1.8),
                Vec3::X,
                DirectionSet::Four,
            );
            let l1 = spawn_sprite_layer(
                &mut commands,
                &mut meshes,
                &mut materials,
                base,
                &sheet,
                Vec2::new(0.9, 1.8),
                1,
            );
            let l2 = spawn_sprite_layer(
                &mut commands,
                &mut meshes,
                &mut materials,
                base,
                &sheet,
                Vec2::new(0.9, 1.8),
                2,
            );
            state.apply(world);
            (base, l1, l2)
        };
        app.update();
        app.update();
        let z = |e| app.world().get::<Transform>(e).unwrap().translation.z;
        assert!(z(l2) > z(l1) && z(l1) > 0.0);
        let uv = |e: Entity| {
            let h = app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(e)
                .unwrap()
                .0
                .clone();
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&h)
                .unwrap()
                .uv_transform
        };
        let base_quad = app.world().get::<Children>(base).unwrap()[0];
        // Объект смотрит на +X, камера тоже на +X: кадр «к камере» одинаков у основы и слоёв.
        assert_eq!(uv(l1), uv(base_quad));
        assert_eq!(uv(l2), uv(base_quad));
    }
}
