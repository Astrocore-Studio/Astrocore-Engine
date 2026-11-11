pub mod rsi;

use astrocore_math::glam::Vec3;

/// Направление спрайта в порядке RSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    South = 0,
    North = 1,
    East = 2,
    West = 3,
    SouthEast = 4,
    NorthEast = 5,
    SouthWest = 6,
    NorthWest = 7,
}

impl Direction {
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Сколько направлений есть у спрайта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionSet {
    One,
    Four,
    Eight,
}

impl DirectionSet {
    pub const fn from_count(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::One),
            4 => Some(Self::Four),
            8 => Some(Self::Eight),
            _ => None,
        }
    }
}

/// Выбрать направление спрайта.
///
/// * `facing` — куда смотрит объект (проекция на горизонтальную плоскость X/Z);
/// * `object_pos`, `camera_pos` — мировые позиции (ось Y игнорируется).
///
/// Объект, обращённый лицом к камере, — `South`; обращённый вправо на экране — `East`.
/// Если направление не определено (нулевой вектор или камера над объектом),
/// возвращается `South`.
pub fn select_direction(
    facing: Vec3,
    object_pos: Vec3,
    camera_pos: Vec3,
    set: DirectionSet,
) -> Direction {
    if set == DirectionSet::One {
        return Direction::South;
    }
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
    let to_camera = flat(camera_pos - object_pos);
    let facing = flat(facing);
    if to_camera.length_squared() < 1e-12 || facing.length_squared() < 1e-12 {
        return Direction::South;
    }
    let to_camera = to_camera.normalize();
    let facing = facing.normalize();
    // Экранное «вправо» при взгляде из камеры на объект: (взгляд) × (вверх).
    let right = (-to_camera).cross(Vec3::Y);
    let angle = facing.dot(right).atan2(facing.dot(to_camera)); // 0 = лицом к камере, + = вправо
    let pi = std::f32::consts::PI;
    match set {
        DirectionSet::Four => {
            // Сектора по 90° с центрами в 0, ±90°, 180°.
            if angle.abs() <= pi * 0.25 {
                Direction::South
            } else if angle.abs() >= pi * 0.75 {
                Direction::North
            } else if angle > 0.0 {
                Direction::East
            } else {
                Direction::West
            }
        }
        _ => {
            // Сектора по 45° с центрами в 0, ±45°, ±90°, ±135°, 180°.
            let sector = ((angle / (pi * 0.25)).round() as i32).rem_euclid(8);
            match sector {
                0 => Direction::South,
                1 => Direction::SouthEast,
                2 => Direction::East,
                3 => Direction::NorthEast,
                4 => Direction::North,
                5 => Direction::NorthWest,
                6 => Direction::West,
                _ => Direction::SouthWest,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJ: Vec3 = Vec3::ZERO;
    // Камера на +Z смотрит на объект вдоль −Z; экранное «вправо» = +X.
    const CAM: Vec3 = Vec3::new(0.0, 5.0, 10.0);

    fn dir4(facing: Vec3) -> Direction {
        select_direction(facing, OBJ, CAM, DirectionSet::Four)
    }

    #[test]
    fn four_directions_relative_to_camera() {
        assert_eq!(dir4(Vec3::Z), Direction::South, "лицом к камере");
        assert_eq!(dir4(Vec3::NEG_Z), Direction::North, "спиной к камере");
        assert_eq!(dir4(Vec3::X), Direction::East, "вправо на экране");
        assert_eq!(dir4(Vec3::NEG_X), Direction::West);
    }

    #[test]
    fn eight_directions_relative_to_camera() {
        let d = |v: Vec3| select_direction(v, OBJ, CAM, DirectionSet::Eight);
        assert_eq!(d(Vec3::new(1.0, 0.0, 1.0)), Direction::SouthEast);
        assert_eq!(d(Vec3::new(1.0, 0.0, -1.0)), Direction::NorthEast);
        assert_eq!(d(Vec3::new(-1.0, 0.0, 1.0)), Direction::SouthWest);
        assert_eq!(d(Vec3::new(-1.0, 0.0, -1.0)), Direction::NorthWest);
        assert_eq!(d(Vec3::Z), Direction::South);
        assert_eq!(d(Vec3::NEG_Z), Direction::North);
    }

    #[test]
    fn selection_follows_camera_orbit() {
        // Камера обошла объект на 90° (теперь на +X): объект, смотревший на +Z, для неё «влево».
        let cam = Vec3::new(10.0, 5.0, 0.0);
        let d = select_direction(Vec3::Z, OBJ, cam, DirectionSet::Four);
        assert_eq!(d, Direction::West);
        // А тот, кто смотрит на камеру (+X), — South.
        assert_eq!(
            select_direction(Vec3::X, OBJ, cam, DirectionSet::Four),
            Direction::South
        );
    }

    #[test]
    fn height_of_facing_and_camera_is_ignored() {
        let a = select_direction(
            Vec3::new(0.0, 7.0, 1.0),
            OBJ,
            Vec3::new(0.0, 100.0, 10.0),
            DirectionSet::Four,
        );
        assert_eq!(a, Direction::South);
    }

    #[test]
    fn degenerate_inputs_fall_back_to_south() {
        assert_eq!(
            select_direction(Vec3::ZERO, OBJ, CAM, DirectionSet::Four),
            Direction::South
        );
        assert_eq!(
            select_direction(Vec3::X, OBJ, Vec3::new(0.0, 9.0, 0.0), DirectionSet::Eight),
            Direction::South
        );
        assert_eq!(
            select_direction(Vec3::X, OBJ, CAM, DirectionSet::One),
            Direction::South
        );
    }

    #[test]
    fn direction_count_parsing() {
        assert_eq!(DirectionSet::from_count(4), Some(DirectionSet::Four));
        assert_eq!(DirectionSet::from_count(2), None);
        assert_eq!(Direction::NorthWest.index(), 7);
    }
}
