use astrocore_math::glam::Vec3;

/// Ось гизмо перемещения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    pub fn dir(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }
}

/// Луч из камеры через точку курсора.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

/// Ближайшие точки двух прямых: параметр на первой (`p + s·d`) для прямой `q + t·e`.
fn closest_param(p: Vec3, d: Vec3, q: Vec3, e: Vec3) -> Option<(f32, f32)> {
    let w = p - q;
    let (a, b, c) = (d.dot(d), d.dot(e), e.dot(e));
    let (dd, ee) = (d.dot(w), e.dot(w));
    let denom = a * c - b * b;
    if denom.abs() < 1e-8 {
        return None; // параллельны
    }
    Some(((b * ee - c * dd) / denom, (a * ee - b * dd) / denom))
}

/// Выбрать ось под курсором: отрезок оси длиной `length` от `origin`, допуск `tolerance` (м).
/// Побеждает ось, ближайшая к лучу; при равенстве — первая по порядку X, Y, Z.
pub fn pick_axis(origin: Vec3, ray: Ray, length: f32, tolerance: f32) -> Option<Axis> {
    let mut best: Option<(Axis, f32)> = None;
    for axis in Axis::ALL {
        let Some((s, t)) = closest_param(origin, axis.dir(), ray.origin, ray.dir) else {
            continue;
        };
        if !(0.0..=length).contains(&s) || t < 0.0 {
            continue;
        }
        let dist = (origin + axis.dir() * s - (ray.origin + ray.dir * t)).length();
        if dist <= tolerance && best.is_none_or(|(_, d)| dist < d - 1e-6) {
            best = Some((axis, dist));
        }
    }
    best.map(|(a, _)| a)
}

/// Перетаскивание по оси: смещение считается от точки захвата, поэтому объект не «прыгает».
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisDrag {
    pub axis: Axis,
    pub origin: Vec3,
    grab: f32,
}

impl AxisDrag {
    pub fn begin(axis: Axis, origin: Vec3, ray: Ray) -> Option<Self> {
        let (s, _) = closest_param(origin, axis.dir(), ray.origin, ray.dir)?;
        Some(Self {
            axis,
            origin,
            grab: s,
        })
    }

    /// Смещение объекта вдоль оси для текущего луча.
    pub fn delta(&self, ray: Ray) -> Vec3 {
        let Some((s, _)) = closest_param(self.origin, self.axis.dir(), ray.origin, ray.dir) else {
            return Vec3::ZERO;
        };
        self.axis.dir() * (s - self.grab)
    }
}

/// Угол поворота вокруг +Y (градусы, против часовой стрелки сверху), на который указывает
/// луч на горизонтальной плоскости через `origin`. `None`, если луч параллелен плоскости.
pub fn yaw_toward(origin: Vec3, ray: Ray) -> Option<f32> {
    if ray.dir.y.abs() < 1e-6 {
        return None;
    }
    let t = (origin.y - ray.origin.y) / ray.dir.y;
    if t < 0.0 {
        return None;
    }
    let p = ray.origin + ray.dir * t - origin;
    // Нулевой угол — направление −Z (вперёд); положительный — против часовой стрелки сверху.
    Some((-p.x).atan2(-p.z).to_degrees())
}

/// Привязать значение к шагу (`step <= 0` — без привязки).
pub fn snap(value: f32, step: f32) -> f32 {
    if step <= 0.0 {
        value
    } else {
        (value / step).round() * step
    }
}
