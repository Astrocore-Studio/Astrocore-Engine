use astrocore_math::ChunkCoord;
use astrocore_terrain::Terrain;
use bevy::prelude::*;

use crate::to_bevy_mesh;

/// Рельеф для отрисовки. Клиент (сеть) или редактор обновляет его и заполняет `dirty`.
#[derive(Resource)]
pub struct RenderedTerrain {
    pub terrain: Terrain,
    pub dirty: Vec<ChunkCoord>,
    pub rebuild_all: bool,
}

impl RenderedTerrain {
    pub fn new(terrain: Terrain) -> Self {
        Self {
            terrain,
            dirty: Vec::new(),
            rebuild_all: true,
        }
    }
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainChunkMesh(pub ChunkCoord);

#[derive(Component)]
pub struct WaterPlane;

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, rebuild_terrain);
    }
}

/// Сколько чанков рельефа пересобирать за кадр.
pub const TERRAIN_CHUNKS_PER_FRAME: usize = 16;

fn rebuild_terrain(
    mut commands: Commands,
    rendered: Option<ResMut<RenderedTerrain>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<(Entity, &TerrainChunkMesh)>,
    water: Query<Entity, With<WaterPlane>>,
) {
    let Some(mut rendered) = rendered else { return };
    let mut gone: std::collections::HashSet<Entity> = std::collections::HashSet::new();
    if rendered.rebuild_all {
        for (e, _) in &existing {
            if gone.insert(e) {
                commands.entity(e).despawn();
            }
        }
        for e in &water {
            commands.entity(e).despawn();
        }
        rendered.dirty = rendered.terrain.populated_chunks();
        rendered.rebuild_all = false;
        if let Some(level) = rendered.terrain.water_level() {
            let chunks = rendered.dirty.clone();
            if !chunks.is_empty() {
                let (lo_x, hi_x) = (
                    chunks.iter().map(|c| c.x).min().unwrap_or(0),
                    chunks.iter().map(|c| c.x).max().unwrap_or(0),
                );
                let (lo_z, hi_z) = (
                    chunks.iter().map(|c| c.z).min().unwrap_or(0),
                    chunks.iter().map(|c| c.z).max().unwrap_or(0),
                );
                let size = rendered.terrain.resolution() * 16.0;
                let (w, d) = (
                    (hi_x - lo_x + 3) as f32 * size,
                    (hi_z - lo_z + 3) as f32 * size,
                );
                let (cx, cz) = (
                    (lo_x + hi_x + 1) as f32 * 0.5 * size,
                    (lo_z + hi_z + 1) as f32 * 0.5 * size,
                );
                commands.spawn((
                    Mesh3d(meshes.add(Cuboid::new(w, 0.05, d))),
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.2, 0.45, 0.75, 0.6),
                        alpha_mode: AlphaMode::Blend,
                        perceptual_roughness: 0.2,
                        ..default()
                    })),
                    Transform::from_xyz(cx, level, cz),
                    WaterPlane,
                ));
            }
        }
    }
    let take = rendered.dirty.len().min(TERRAIN_CHUNKS_PER_FRAME);
    let batch: Vec<ChunkCoord> = rendered.dirty.drain(..take).collect();
    for chunk in batch {
        for (e, c) in &existing {
            if c.0 == chunk && gone.insert(e) {
                commands.entity(e).despawn();
            }
        }
        let mesh = rendered.terrain.build_chunk_mesh(chunk);
        if mesh.is_empty() {
            continue;
        }
        commands.spawn((
            Mesh3d(meshes.add(to_bevy_mesh(&mesh))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.36, 0.5, 0.3),
                perceptual_roughness: 1.0,
                ..default()
            })),
            Transform::IDENTITY,
            TerrainChunkMesh(chunk),
        ));
    }
}
