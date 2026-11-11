use glam::{DVec3, Vec3};

/// Глобальная позиция в мире, метры, двойная точность: у далёких от начала координат объектов
/// f32 теряет миллиметры, поэтому хранение глобальное, а вычисления — локальные.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WorldPos(pub DVec3);

impl WorldPos {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self(DVec3::new(x, y, z))
    }
}

/// Плавающее начало координат: локальные f32-позиции отсчитываются от `origin`. Когда
/// наблюдатель уходит дальше `threshold`, начало переносится в ближайший узел сетки
/// шага `snap` — кратный размеру чанка, чтобы границы чанков в локальных координатах
/// не «плыли».
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatingOrigin {
    origin: DVec3,
    threshold: f32,
    snap: f64,
}

impl FloatingOrigin {
    /// `threshold` — дальность до переноса, м; `snap` — шаг узлов начала (обычно размер чанка).
    pub fn new(threshold: f32, snap: f32) -> Self {
        Self {
            origin: DVec3::ZERO,
            threshold: threshold.max(1.0),
            snap: f64::from(snap.max(1.0)),
        }
    }

    pub fn origin(&self) -> WorldPos {
        WorldPos(self.origin)
    }

    pub fn to_local(&self, p: WorldPos) -> Vec3 {
        (p.0 - self.origin).as_vec3()
    }

    pub fn to_global(&self, local: Vec3) -> WorldPos {
        WorldPos(self.origin + local.as_dvec3())
    }

    /// Нужно ли переносить начало для наблюдателя в локальной точке (по горизонтали и по Y).
    pub fn needs_rebase(&self, observer_local: Vec3) -> bool {
        observer_local.abs().max_element() > self.threshold
    }

    /// Перенести начало к наблюдателю. Возвращает сдвиг, который нужно **вычесть** из локальных
    /// позиций всех объектов; `None`, если перенос не нужен.
    pub fn rebase_to(&mut self, observer_local: Vec3) -> Option<Vec3> {
        if !self.needs_rebase(observer_local) {
            return None;
        }
        let target = self.origin + observer_local.as_dvec3();
        let snapped = (target / self.snap).round() * self.snap;
        let shift = (snapped - self.origin).as_vec3();
        if shift == Vec3::ZERO {
            return None;
        }
        self.origin = snapped;
        Some(shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_round_trip_far_from_zero_keeps_millimetres() {
        let mut o = FloatingOrigin::new(1000.0, 32.0);
        let far = WorldPos::new(8_000_000.4, 12.0, -6_000_000.7);
        // Без переноса f32-локальная позиция огрубляется до метров.
        let coarse = o.to_local(far);
        assert!((o.to_global(coarse).0 - far.0).length() > 0.1);
        // После переноса начала к наблюдателю точность возвращается.
        let shift = o.rebase_to(coarse).unwrap();
        assert!(shift.length() > 1.0e6);
        let local = o.to_local(far);
        assert!(local.abs().max_element() < 32.0, "{local}");
        assert!((o.to_global(local).0 - far.0).length() < 1e-3);
    }

    #[test]
    fn rebase_happens_only_beyond_the_threshold_and_snaps_to_chunks() {
        let mut o = FloatingOrigin::new(500.0, 32.0);
        assert!(o.rebase_to(Vec3::new(499.0, 0.0, 0.0)).is_none());
        assert!(!o.needs_rebase(Vec3::new(0.0, 499.0, 0.0)));
        let shift = o.rebase_to(Vec3::new(600.5, 3.0, -10.0)).unwrap();
        let org = o.origin().0;
        assert_eq!(org.x % 32.0, 0.0);
        assert_eq!(org.y % 32.0, 0.0);
        assert_eq!(org.z % 32.0, 0.0);
        assert_eq!(shift.x, org.x as f32);
        // Позиция объекта в мире не меняется: старая локальная минус сдвиг = новая локальная.
        let before = WorldPos::new(600.5, 3.0, -10.0);
        let after_local = o.to_local(before);
        assert!((after_local - (Vec3::new(600.5, 3.0, -10.0) - shift)).length() < 1e-3);
        // Повторный перенос на том же месте не нужен.
        assert!(o.rebase_to(Vec3::new(5.0, 0.0, 0.0)).is_none());
    }
}
