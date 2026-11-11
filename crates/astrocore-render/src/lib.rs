//! Клиентский рендер Astrocore поверх Bevy (раздел 12 дизайн-документа).
//!
//! * [`WorldMeshPlugin`] — меши структуры мира по чанкам: пересборка только
//!   затронутых (чанк × этаж) регионов по dirty-спискам, батчинг по материалам;
//! * [`billboard`] — направленные спрайты (directional billboards): вертикальный
//!   квадрат, обращённый к камере вокруг оси Y; кадр выбирается по повороту объекта
//!   относительно камеры ([`astrocore_sprites::select_direction`]);
//! * [`camera`] — перспективная и ортографическая камеры.
//!
//! Модуль не владеет миром: клиент (сеть) или редактор обновляет [`RenderedWorld`] и
//! добавляет регионы в [`RenderedWorld::dirty`].

pub mod billboard;
pub mod camera;
pub mod meshes;

use std::collections::HashMap;

use astrocore_core::PrototypeId;
use astrocore_world::{DirtyRegion, World};
use bevy::prelude::*;

pub use billboard::{BillboardPlugin, DirectionalSprite, SpriteSheet};
pub use camera::{CameraMode, CameraRig, CameraRigPlugin};
pub use meshes::{material_color, to_bevy_mesh};

/// Мир, который нужно отрисовать, и накопленные регионы для пересборки.
#[derive(Resource)]
pub struct RenderedWorld {
    pub world: World,
    /// Регионы, которые нужно пересобрать (получены из `World::take_dirty` или от сети).
    pub dirty: Vec<DirtyRegion>,
    /// Пересобрать всё (первый кадр, смена мира).
    pub rebuild_all: bool,
}

impl RenderedWorld {
    pub fn new(world: World) -> Self {
        Self {
            world,
            dirty: Vec::new(),
            rebuild_all: true,
        }
    }
}

/// Кэш материалов по идентификатору прототипа.
#[derive(Resource, Default)]
pub struct MaterialCache {
    handles: HashMap<PrototypeId, Handle<StandardMaterial>>,
}

impl MaterialCache {
    pub fn get_or_create(
        &mut self,
        id: &PrototypeId,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.handles
            .entry(id.clone())
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: material_color(id),
                    perceptual_roughness: 0.9,
                    ..default()
                })
            })
            .clone()
    }
}

/// Маркер сущности меша региона.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionMesh {
    pub chunk: astrocore_math::ChunkCoord,
    pub floor: astrocore_world::FloorId,
}

pub struct WorldMeshPlugin;

impl Plugin for WorldMeshPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MaterialCache>()
            .add_systems(Update, rebuild_regions);
    }
}

/// Сколько регионов пересобирать за кадр (остальное — в следующих кадрах).
pub const REGIONS_PER_FRAME: usize = 64;

fn rebuild_regions(
    mut commands: Commands,
    mut rendered: Option<ResMut<RenderedWorld>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<MaterialCache>,
    existing: Query<(Entity, &RegionMesh)>,
) {
    let Some(rendered) = rendered.as_mut() else {
        return;
    };
    if rendered.rebuild_all {
        for (e, _) in &existing {
            commands.entity(e).despawn();
        }
        let floors: Vec<_> = rendered.world.floors().map(|f| f.id).collect();
        let mut all = Vec::new();
        for floor in floors {
            for chunk in rendered.world.populated_chunks(floor) {
                all.push(DirtyRegion {
                    chunk,
                    floor,
                    flags: astrocore_world::DirtyFlags::GEOMETRY,
                });
            }
        }
        rendered.dirty = all;
        rendered.rebuild_all = false;
    }
    let take = rendered.dirty.len().min(REGIONS_PER_FRAME);
    let batch: Vec<DirtyRegion> = rendered.dirty.drain(..take).collect();
    for region in batch {
        if !region.flags.contains(astrocore_world::DirtyFlags::GEOMETRY) {
            continue;
        }
        // Старые меши региона удаляются, новые строятся из канонического мира.
        for (e, r) in &existing {
            if r.chunk == region.chunk && r.floor == region.floor {
                commands.entity(e).despawn();
            }
        }
        spawn_freeform_proxies(
            &mut commands,
            &rendered.world,
            region,
            &mut meshes,
            &mut materials,
            &mut cache,
        );
        for (material_id, data) in
            astrocore_geometry::build_chunk_meshes(&rendered.world, region.chunk, region.floor)
        {
            if data.is_empty() {
                continue;
            }
            commands.spawn((
                Mesh3d(meshes.add(to_bevy_mesh(&data))),
                MeshMaterial3d(cache.get_or_create(&material_id, &mut materials)),
                Transform::IDENTITY,
                RegionMesh {
                    chunk: region.chunk,
                    floor: region.floor,
                },
            ));
        }
    }
}

/// Свободные модели. Пока ассеты билда не подключены, вместо модели рисуется её габаритный
/// ящик (ориентир для мапперов и проверка размещения); GLB подставляется загрузчиком ассетов.
fn spawn_freeform_proxies(
    commands: &mut Commands,
    world: &World,
    region: DirtyRegion,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
) {
    use astrocore_world::{Placement, StructureKind};
    let cfg = world.config();
    for e in world.elements_in_chunk(region.chunk, region.floor) {
        let (StructureKind::Freeform, Placement::Free(f)) = (e.kind, e.placement) else {
            continue;
        };
        // Один раз: у чанка-владельца (якорная ячейка лежит ровно в одном чанке).
        if f.cell.chunk(cfg.grid_step_m, cfg.chunk_size_m) != region.chunk {
            continue;
        }
        let p = f.position(cfg.grid_step_m);
        let half = f.half_mm.map(|h| h as f32 / 1000.0);
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(half[0] * 2.0, half[1] * 2.0, half[2] * 2.0))),
            MeshMaterial3d(cache.get_or_create(&e.material, materials)),
            Transform {
                translation: Vec3::new(p[0], p[1] + half[1], p[2]),
                rotation: Quat::from_rotation_y(f.yaw_rad()),
                scale: Vec3::ONE,
            },
            RegionMesh {
                chunk: region.chunk,
                floor: region.floor,
            },
        ));
    }
}
