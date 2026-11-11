use std::collections::HashMap;

use astrocore_core::PrototypeId;
use bevy::prelude::*;

/// Источник света как данные сцены. Движок сам превращает его в Bevy-свет и следит за
/// настройками теней, поэтому игровой код не зависит от конкретных компонентов Bevy.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct LightSource {
    pub kind: LightKind,
    pub color: Color,
    /// Сила света, люмен.
    pub intensity: f32,
    /// Радиус действия, м.
    pub range: f32,
    pub cast_shadows: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightKind {
    Point,
    /// Прожектор; угол полураствора в радианах.
    Spot {
        half_angle: f32,
    },
}

impl LightSource {
    pub fn point(color: Color, intensity: f32, range: f32) -> Self {
        Self {
            kind: LightKind::Point,
            color,
            intensity,
            range,
            cast_shadows: true,
        }
    }
}

/// Глобальные настройки теней: качество выбирается по бенчмаркам, поэтому это ресурс.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ShadowSettings {
    pub sun: bool,
    pub local_lights: bool,
    /// Не больше стольких теневых локальных источников (дальше выключаются).
    pub max_shadow_lights: usize,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            sun: true,
            local_lights: true,
            max_shadow_lights: 4,
        }
    }
}

/// Излучающие материалы: идентификатор прототипа → цвет и сила свечения.
#[derive(Resource, Default, Debug, Clone)]
pub struct EmissiveMaterials(pub HashMap<PrototypeId, (Color, f32)>);

impl EmissiveMaterials {
    pub fn set(&mut self, id: PrototypeId, color: Color, strength: f32) {
        self.0.insert(id, (color, strength));
    }

    pub fn linear(&self, id: &PrototypeId) -> Option<LinearRgba> {
        self.0.get(id).map(|(c, k)| {
            let l = c.to_linear();
            LinearRgba::rgb(l.red * k, l.green * k, l.blue * k)
        })
    }
}

pub struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShadowSettings>()
            .init_resource::<EmissiveMaterials>()
            .add_systems(Update, (sync_light_sources, apply_sun_shadows));
    }
}

/// Создаёт Bevy-свет для каждого `LightSource` и обновляет его при изменении.
fn sync_light_sources(
    mut commands: Commands,
    settings: Res<ShadowSettings>,
    sources: Query<(Entity, Ref<LightSource>)>,
) {
    // Ближе к началу списка — выше приоритет теней; порядок детерминирован по Entity.
    let mut ordered: Vec<_> = sources.iter().collect();
    ordered.sort_by_key(|(e, _)| e.index());
    let mut shadow_budget = settings.max_shadow_lights;
    for (entity, src) in ordered {
        let shadows = src.cast_shadows && settings.local_lights && shadow_budget > 0;
        if shadows {
            shadow_budget -= 1;
        }
        if !(src.is_changed() || settings.is_changed()) {
            continue;
        }
        let mut e = commands.entity(entity);
        e.remove::<(PointLight, SpotLight)>();
        match src.kind {
            LightKind::Point => {
                e.insert(PointLight {
                    color: src.color,
                    intensity: src.intensity,
                    range: src.range,
                    shadow_maps_enabled: shadows,
                    ..default()
                });
            }
            LightKind::Spot { half_angle } => {
                e.insert(SpotLight {
                    color: src.color,
                    intensity: src.intensity,
                    range: src.range,
                    outer_angle: half_angle,
                    inner_angle: half_angle * 0.8,
                    shadow_maps_enabled: shadows,
                    ..default()
                });
            }
        }
    }
}

fn apply_sun_shadows(settings: Res<ShadowSettings>, mut suns: Query<&mut DirectionalLight>) {
    if !settings.is_changed() {
        return;
    }
    for mut sun in &mut suns {
        sun.shadow_maps_enabled = settings.sun;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emissive_scales_linear_color() {
        let mut m = EmissiveMaterials::default();
        let id = PrototypeId::new("t:lamp").unwrap();
        assert!(m.linear(&id).is_none());
        m.set(id.clone(), Color::linear_rgb(1.0, 0.5, 0.0), 4.0);
        let l = m.linear(&id).unwrap();
        assert!((l.red - 4.0).abs() < 1e-5 && (l.green - 2.0).abs() < 1e-5 && l.blue == 0.0);
    }

    #[test]
    fn lights_become_bevy_lights_with_a_shadow_budget() {
        let mut app = App::new();
        app.add_plugins(LightingPlugin);
        app.insert_resource(ShadowSettings {
            max_shadow_lights: 1,
            ..default()
        });
        let a = app
            .world_mut()
            .spawn(LightSource::point(Color::WHITE, 1000.0, 8.0))
            .id();
        let b = app
            .world_mut()
            .spawn(LightSource {
                kind: LightKind::Spot { half_angle: 0.6 },
                ..LightSource::point(Color::WHITE, 500.0, 12.0)
            })
            .id();
        app.update();
        let p = app.world().get::<PointLight>(a).unwrap();
        assert_eq!((p.shadow_maps_enabled, p.range), (true, 8.0));
        let s = app.world().get::<SpotLight>(b).unwrap();
        assert!(!s.shadow_maps_enabled, "лимит теневых источников исчерпан");
        // Выключение теней применяется ко всем.
        app.world_mut()
            .resource_mut::<ShadowSettings>()
            .local_lights = false;
        app.update();
        assert!(
            !app.world()
                .get::<PointLight>(a)
                .unwrap()
                .shadow_maps_enabled
        );
    }
}
