use astrocore_math::glam::Vec3;
use astrocore_physics::{
    BoxWorld, CharacterShape, CharacterState, GroundProvider, move_character,
    move_character_on_ground,
};

/// Скорость ходьбы, м/с.
pub const MOVE_SPEED: f32 = 4.0;

/// Команда ввода одного тика.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputCmd {
    pub seq: u32,
    pub move_x: i8,
    pub move_z: i8,
    pub yaw: f32,
    /// Полёт сквозь преграды без тяжести (призрак). Сервер решает сам; клиент включает у себя, когда
    /// знает, что он призрак, чтобы предсказание совпадало.
    pub noclip: bool,
}

/// Желаемое направление в мировых осях. `yaw` — поворот вокруг +Y (радианы);
/// `move_z = +1` — вперёд (по взгляду, при yaw = 0 это −Z), `move_x = +1` — вправо.
pub fn wish_direction(move_x: i8, move_z: i8, yaw: f32) -> Vec3 {
    let (sin, cos) = yaw.sin_cos();
    let forward = Vec3::new(-sin, 0.0, -cos);
    let right = Vec3::new(cos, 0.0, -sin);
    let v = right * f32::from(move_x.clamp(-1, 1)) + forward * f32::from(move_z.clamp(-1, 1));
    v.normalize_or_zero()
}

/// Применить команду ввода за `dt` секунд. С рельефом (`ground`) персонаж стоит на земле;
/// сервер и клиент обязаны передавать одинаковый рельеф — иначе предсказание разойдётся.
pub fn apply_input(
    bw: &BoxWorld,
    ground: Option<&dyn GroundProvider>,
    state: &mut CharacterState,
    shape: &CharacterShape,
    input: &InputCmd,
    dt: f32,
) {
    let wish = wish_direction(input.move_x, input.move_z, input.yaw) * MOVE_SPEED * dt;
    if input.noclip {
        state.pos += wish;
        state.vel_y = 0.0;
        state.grounded = false;
        return;
    }
    match ground {
        Some(g) => move_character_on_ground(bw, g, state, shape, wish, dt),
        None => move_character(bw, state, shape, wish, dt),
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn forward_follows_yaw() {
        assert!(near(wish_direction(0, 1, 0.0), Vec3::NEG_Z));
        assert!(near(wish_direction(1, 0, 0.0), Vec3::X));
        // Поворот на 90° против часовой стрелки сверху: «вперёд» смотрит на −X.
        assert!(near(wish_direction(0, 1, FRAC_PI_2), Vec3::NEG_X));
        assert!(near(wish_direction(1, 0, FRAC_PI_2), Vec3::NEG_Z));
    }

    #[test]
    fn diagonal_is_not_faster_and_idle_is_zero() {
        assert!((wish_direction(1, 1, 0.3).length() - 1.0).abs() < 1e-5);
        assert_eq!(wish_direction(0, 0, 1.0), Vec3::ZERO);
        // Значения вне диапазона не ускоряют движение.
        assert!((wish_direction(100, 0, 0.0).length() - 1.0).abs() < 1e-5);
    }
}
