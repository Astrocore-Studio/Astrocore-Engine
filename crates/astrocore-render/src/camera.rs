use bevy::camera::{OrthographicProjection, Projection, ScalingMode};
use bevy::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    Perspective,
    Orthographic,
}

/// Параметры камеры. Положение вычисляется из них системой [`CameraRigPlugin`].
#[derive(Component, Debug, Clone)]
pub struct CameraRig {
    pub mode: CameraMode,
    /// Точка, на которую смотрит камера (для перспективы от первого лица — положение глаз).
    pub target: Vec3,
    /// Поворот вокруг Y, радианы.
    pub yaw: f32,
    /// Наклон: 0 — горизонт, положительный — взгляд вниз.
    pub pitch: f32,
    /// Расстояние от цели (0 — камера в глазах).
    pub distance: f32,
    /// Высота видимой области для ортографической камеры, метры.
    pub ortho_height: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            mode: CameraMode::Perspective,
            target: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.4,
            distance: 8.0,
            ortho_height: 20.0,
        }
    }
}

impl CameraRig {
    /// Положение и поворот камеры: смотрит на `target` с расстояния `distance`.
    pub fn transform(&self) -> Transform {
        let rotation = Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(-self.pitch);
        let position = self.target + rotation * Vec3::new(0.0, 0.0, self.distance);
        Transform {
            translation: position,
            rotation,
            scale: Vec3::ONE,
        }
    }
}

pub struct CameraRigPlugin;

impl Plugin for CameraRigPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, apply_rig.before(TransformSystems::Propagate));
    }
}

fn apply_rig(mut cams: Query<(&CameraRig, &mut Transform, &mut Projection)>) {
    for (rig, mut transform, mut projection) in &mut cams {
        *transform = rig.transform();
        let want_ortho = rig.mode == CameraMode::Orthographic;
        let is_ortho = matches!(*projection, Projection::Orthographic(_));
        if want_ortho && !is_ortho {
            *projection = Projection::Orthographic(OrthographicProjection {
                scaling_mode: ScalingMode::FixedVertical {
                    viewport_height: rig.ortho_height,
                },
                ..OrthographicProjection::default_3d()
            });
        } else if !want_ortho && is_ortho {
            *projection = Projection::Perspective(PerspectiveProjection::default());
        } else if let Projection::Orthographic(o) = &mut *projection {
            o.scaling_mode = ScalingMode::FixedVertical {
                viewport_height: rig.ortho_height,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_looks_at_target_from_distance() {
        let rig = CameraRig {
            target: Vec3::new(5.0, 1.0, 5.0),
            yaw: 0.0,
            pitch: 0.0,
            distance: 10.0,
            ..default()
        };
        let t = rig.transform();
        assert!((t.translation - Vec3::new(5.0, 1.0, 15.0)).length() < 1e-4);
        assert!(
            (t.rotation * Vec3::NEG_Z - Vec3::NEG_Z).length() < 1e-5,
            "смотрит вдоль −Z на цель"
        );
    }

    #[test]
    fn positive_pitch_looks_down() {
        let rig = CameraRig {
            pitch: 0.7,
            distance: 5.0,
            ..default()
        };
        let t = rig.transform();
        assert!(t.translation.y > 0.0, "камера выше цели");
        assert!((t.rotation * Vec3::NEG_Z).y < 0.0, "взгляд направлен вниз");
        let to_target = (rig.target - t.translation).normalize();
        assert!(((t.rotation * Vec3::NEG_Z) - to_target).length() < 1e-4);
    }

    #[test]
    fn yaw_orbits_around_the_target() {
        let rig = CameraRig {
            yaw: std::f32::consts::FRAC_PI_2,
            pitch: 0.0,
            distance: 4.0,
            ..default()
        };
        let t = rig.transform();
        assert!(
            (t.translation - Vec3::new(4.0, 0.0, 0.0)).length() < 1e-4,
            "{:?}",
            t.translation
        );
    }
}
