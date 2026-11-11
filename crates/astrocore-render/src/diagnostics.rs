use bevy::prelude::*;

/// Счётчики рендера. Это оценка по сцене Bevy (без GPU-таймеров): видимые объекты,
/// треугольники и число вызовов отрисовки (= видимые меш-сущности; Bevy склеивает их по
/// материалу, так что реальных draw call не больше).
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct RenderStats {
    pub mesh_entities: usize,
    pub visible_objects: usize,
    pub visible_triangles: usize,
    pub total_triangles: usize,
    pub draw_calls_upper_bound: usize,
    pub meshes_in_memory_bytes: usize,
    pub last_frame_ms: f32,
}

/// Диагностика обновляется раз в `every_frames` кадров, чтобы сама не нагружала рендер.
#[derive(Resource, Debug, Clone, Copy)]
pub struct RenderStatsConfig {
    pub every_frames: u32,
}

impl Default for RenderStatsConfig {
    fn default() -> Self {
        Self { every_frames: 30 }
    }
}

pub struct RenderDiagnosticsPlugin;

impl Plugin for RenderDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderStats>()
            .init_resource::<RenderStatsConfig>()
            .add_systems(Last, collect_stats);
    }
}

fn triangles(mesh: &Mesh) -> usize {
    mesh.indices()
        .map_or_else(|| mesh.count_vertices() / 3, |i| i.len() / 3)
}

fn collect_stats(
    time: Res<Time>,
    cfg: Res<RenderStatsConfig>,
    meshes: Res<Assets<Mesh>>,
    q: Query<(&Mesh3d, Option<&ViewVisibility>)>,
    mut stats: ResMut<RenderStats>,
    mut frame: Local<u32>,
) {
    stats.last_frame_ms = time.delta_secs() * 1000.0;
    *frame += 1;
    if !frame.is_multiple_of(cfg.every_frames.max(1)) {
        return;
    }
    let mut s = RenderStats {
        last_frame_ms: stats.last_frame_ms,
        ..default()
    };
    for (mesh, vis) in &q {
        let tris = meshes.get(&mesh.0).map_or(0, triangles);
        s.mesh_entities += 1;
        s.total_triangles += tris;
        if vis.is_none_or(|v| v.get()) {
            s.visible_objects += 1;
            s.visible_triangles += tris;
            s.draw_calls_upper_bound += 1;
        }
    }
    s.meshes_in_memory_bytes = meshes
        .iter()
        .map(|(_, m)| m.count_vertices() * 32 + m.indices().map_or(0, |i| i.len() * 4))
        .sum();
    *stats = s;
}

impl RenderStats {
    pub fn summary(&self) -> String {
        format!(
            "объектов {}/{}, треугольников {}/{}, вызовов ≤ {}, меши {} КиБ, кадр {:.1} мс",
            self.visible_objects,
            self.mesh_entities,
            self.visible_triangles,
            self.total_triangles,
            self.draw_calls_upper_bound,
            self.meshes_in_memory_bytes / 1024,
            self.last_frame_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_entities_and_triangles() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<Assets<Mesh>>()
            .add_plugins(RenderDiagnosticsPlugin)
            .insert_resource(RenderStatsConfig { every_frames: 1 });
        let h = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(1.0, 1.0, 1.0));
        app.world_mut().spawn(Mesh3d(h.clone()));
        app.world_mut().spawn(Mesh3d(h));
        app.update();
        let s = *app.world().resource::<RenderStats>();
        assert_eq!(s.mesh_entities, 2);
        assert_eq!(s.visible_triangles, 24);
        assert!(s.summary().contains("объектов 2/2"));
    }
}
