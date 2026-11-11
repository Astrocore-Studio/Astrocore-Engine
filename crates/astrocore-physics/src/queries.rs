use astrocore_core::ObjectId;
use astrocore_math::Aabb;
use astrocore_math::glam::Vec3;

use crate::{BoxWorld, CharacterShape, CharacterState, RayHit, aabb_overlap, ray_aabb};

/// Слои коллизии: объект принадлежит слою (битовая маска), запрос задаёт маску слоёв, которые
/// его интересуют. Объект виден запросу, если `layer & mask != 0`.
pub const LAYER_STATIC: u32 = 1;
pub const LAYER_DYNAMIC: u32 = 1 << 1;
pub const LAYER_TRIGGER: u32 = 1 << 2;
pub const LAYER_ALL: u32 = u32::MAX;

/// Результат движения коробки: доля пути до первого касания (0..=1), нормаль и точка.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeHit {
    pub id: ObjectId,
    pub fraction: f32,
    pub normal: Vec3,
    pub point: Vec3,
}

/// Контакт перекрывающейся коробки: нормаль, по которой её надо вытолкнуть, и глубина.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub id: ObjectId,
    pub normal: Vec3,
    pub depth: f32,
}

impl BoxWorld {
    /// Назначить слой объекту (двери, платформы, триггеры).
    pub fn set_layer(&mut self, id: ObjectId, layer: u32) {
        self.layers.insert(id, layer);
    }

    pub fn layer_of(&self, id: ObjectId, is_static: bool) -> u32 {
        self.layers.get(&id).copied().unwrap_or(if is_static {
            LAYER_STATIC
        } else {
            LAYER_DYNAMIC
        })
    }

    fn all_boxes<'a>(
        &'a self,
        area: &Aabb,
        mask: u32,
    ) -> impl Iterator<Item = (ObjectId, Aabb)> + 'a {
        let (lo, hi) = self.chunk_range(area);
        let statics = self
            .statics
            .iter()
            .filter(move |((c, _), _)| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z)
            .flat_map(|(_, v)| v.iter())
            .filter(move |(id, _)| self.layer_of(*id, true) & mask != 0)
            .copied();
        let dynamics = self
            .dynamics
            .iter()
            .filter(move |(id, _)| self.layer_of(**id, false) & mask != 0)
            .map(|(id, b)| (*id, *b));
        statics.chain(dynamics)
    }

    /// Луч с маской слоёв.
    pub fn raycast_masked(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        mask: u32,
    ) -> Option<RayHit> {
        let dir = dir.try_normalize()?;
        let end = origin + dir * max_distance;
        let area = Aabb::new(origin.min(end), origin.max(end));
        let mut best: Option<RayHit> = None;
        for (id, b) in self.all_boxes(&area, mask) {
            if let Some((t, normal)) = ray_aabb(origin, dir, &b)
                && t <= max_distance
                && best.is_none_or(|h| t < h.distance)
            {
                best = Some(RayHit {
                    id,
                    distance: t,
                    point: origin + dir * t,
                    normal,
                });
            }
        }
        best
    }

    /// Пересечения коробки с маской слоёв; без повторов, по возрастанию `ObjectId`.
    pub fn overlap_masked(&self, aabb: &Aabb, mask: u32) -> Vec<ObjectId> {
        let mut ids: Vec<ObjectId> = self
            .all_boxes(aabb, mask)
            .filter(|(_, b)| aabb_overlap(aabb, b))
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Протащить коробку на `motion` и найти первое касание (суммирование Минковского:
    /// каждая преграда раздувается на половину размера коробки, центр идёт лучом).
    /// Коробка, уже пересекающая преграду, возвращает касание с `fraction = 0`.
    pub fn shapecast(&self, shape: &Aabb, motion: Vec3, mask: u32) -> Option<ShapeHit> {
        let half = shape.size() * 0.5;
        let center = shape.center();
        let end = shape.translated(motion);
        let area = shape.union(&end);
        let mut best: Option<ShapeHit> = None;
        for (id, b) in self.all_boxes(&area, mask) {
            let grown = Aabb::new(b.min - half, b.max + half);
            let hit = if aabb_overlap(shape, &b) {
                Some((0.0, self.push_normal(shape, &b)))
            } else {
                let len = motion.length();
                if len < 1e-9 {
                    None
                } else {
                    ray_aabb(center, motion / len, &grown)
                        .filter(|(t, _)| *t <= len)
                        .map(|(t, n)| (t / len, n))
                }
            };
            if let Some((fraction, normal)) = hit
                && best.is_none_or(|h| fraction < h.fraction)
            {
                best = Some(ShapeHit {
                    id,
                    fraction,
                    normal,
                    point: center + motion * fraction,
                });
            }
        }
        best
    }

    fn push_normal(&self, shape: &Aabb, other: &Aabb) -> Vec3 {
        penetration(shape, other).map_or(Vec3::Y, |(n, _)| n)
    }

    /// Контакты коробки: на каждое перекрытие — минимальная ось выталкивания и глубина.
    pub fn contacts(&self, shape: &Aabb, mask: u32) -> Vec<Contact> {
        let mut out: Vec<Contact> = self
            .all_boxes(shape, mask)
            .filter_map(|(id, b)| {
                penetration(shape, &b).map(|(normal, depth)| Contact { id, normal, depth })
            })
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Сдвинуть динамическую коробку (платформа, лифт) и вернуть фактическое смещение.
    pub fn move_dynamic(&mut self, id: ObjectId, delta: Vec3) -> Option<Vec3> {
        let b = self.dynamics.get_mut(&id)?;
        *b = b.translated(delta);
        Some(delta)
    }

    pub fn dynamic_box(&self, id: ObjectId) -> Option<Aabb> {
        self.dynamics.get(&id).copied()
    }
}

/// Ось и глубина минимального выталкивания `a` из `b`; `None`, если коробки не пересекаются.
fn penetration(a: &Aabb, b: &Aabb) -> Option<(Vec3, f32)> {
    if !aabb_overlap(a, b) {
        return None;
    }
    let mut best: Option<(Vec3, f32)> = None;
    for axis in 0..3 {
        let to_neg = a.max[axis] - b.min[axis];
        let to_pos = b.max[axis] - a.min[axis];
        let (depth, sign) = if to_neg < to_pos {
            (to_neg, -1.0)
        } else {
            (to_pos, 1.0)
        };
        if best.is_none_or(|(_, d)| depth < d) {
            let mut n = Vec3::ZERO;
            n[axis] = sign;
            best = Some((n, depth));
        }
    }
    best
}

/// Переместить платформу и увлечь всех, кто на ней стоит (ступни на верхней грани, в пределах
/// её проекции). Так движущийся родитель (лифт, подвижный пол) корректно меняет мировую
/// позицию пассажиров. Возвращает, сколько пассажиров увлечено.
pub fn carry_riders(
    world: &mut BoxWorld,
    platform: ObjectId,
    delta: Vec3,
    riders: &mut [CharacterState],
    shape: &CharacterShape,
) -> usize {
    const TOP_EPS: f32 = 0.08;
    let Some(before) = world.dynamic_box(platform) else {
        return 0;
    };
    let mut carried = 0;
    for r in riders.iter_mut() {
        let on_top = (r.pos.y - before.max.y).abs() <= TOP_EPS
            && r.pos.x + shape.radius > before.min.x
            && r.pos.x - shape.radius < before.max.x
            && r.pos.z + shape.radius > before.min.z
            && r.pos.z - shape.radius < before.max.z;
        if on_top {
            r.pos += delta;
            r.vel_y = 0.0;
            r.grounded = true;
            carried += 1;
        }
    }
    world.move_dynamic(platform, delta);
    carried
}
