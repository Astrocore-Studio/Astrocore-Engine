use astrocore_math::ChunkCoord;
use bevy::prelude::*;

use crate::{RegionMesh, RenderedWorld};

/// Дальность отрисовки (клиентский стриминг видимого): регионы мира дальше `max_distance`
/// от камеры скрываются. Скрытие обратимо и не трогает мир — это решение клиента.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct DrawDistance {
    pub max_distance: f32,
}

impl Default for DrawDistance {
    fn default() -> Self {
        Self {
            max_distance: 160.0,
        }
    }
}

pub struct DrawDistancePlugin;

impl Plugin for DrawDistancePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DrawDistance>()
            .add_systems(PostUpdate, cull_regions.before(TransformSystems::Propagate));
    }
}

/// Горизонтальное расстояние от точки до ближайшей границы чанка (0 внутри чанка).
pub fn distance_to_chunk(point: Vec3, chunk: ChunkCoord, chunk_size: f32) -> f32 {
    let min = chunk.min_corner(chunk_size);
    let dx = (min.x - point.x)
        .max(point.x - (min.x + chunk_size))
        .max(0.0);
    let dz = (min.z - point.z)
        .max(point.z - (min.z + chunk_size))
        .max(0.0);
    (dx * dx + dz * dz).sqrt()
}

fn cull_regions(
    distance: Res<DrawDistance>,
    rendered: Option<Res<RenderedWorld>>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut regions: Query<(&RegionMesh, &mut Visibility)>,
) {
    let (Some(rendered), Ok(cam)) = (rendered, camera.single()) else {
        return;
    };
    let size = rendered.world.config().chunk_size_m;
    let cam = cam.translation();
    for (region, mut vis) in &mut regions {
        let far = distance_to_chunk(cam, region.chunk, size) > distance.max_distance;
        let want = if far {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if *vis != want {
            *vis = want;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_distance_is_zero_inside_and_grows_outside() {
        let c = ChunkCoord::new(0, 0);
        assert_eq!(distance_to_chunk(Vec3::new(5.0, 9.0, 5.0), c, 32.0), 0.0);
        assert!((distance_to_chunk(Vec3::new(40.0, 0.0, 5.0), c, 32.0) - 8.0).abs() < 1e-5);
        assert!((distance_to_chunk(Vec3::new(-3.0, 0.0, -4.0), c, 32.0) - 5.0).abs() < 1e-5);
        // Отрицательные чанки.
        let n = ChunkCoord::new(-1, -1);
        assert_eq!(
            distance_to_chunk(Vec3::new(-10.0, 0.0, -10.0), n, 32.0),
            0.0
        );
    }
}
