use std::collections::{BTreeMap, BTreeSet, HashMap};

use astrocore_core::ObjectId;
use astrocore_geometry::{collision_boxes, opening_bounds};
use astrocore_math::glam::Vec3;
use astrocore_math::{Aabb, ChunkCoord};
use astrocore_world::{DirtyRegion, FloorId, OpeningKind, StructureKind, World};

pub mod character;
pub mod queries;

pub use character::{
    CharacterShape, CharacterState, GroundProvider, move_character, move_character_on_ground,
};
pub use queries::{
    Contact, LAYER_ALL, LAYER_DYNAMIC, LAYER_STATIC, LAYER_TRIGGER, ShapeHit, carry_riders,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    pub id: ObjectId,
    pub distance: f32,
    pub point: Vec3,
    pub normal: Vec3,
}

/// Пространственные запросы, не зависящие от реализации физики.
pub trait SpatialQuery {
    /// Ближайшее пересечение луча (направление нормализуется внутри).
    fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<RayHit>;
    /// Все объекты, пересекающие коробку, без повторов, по возрастанию `ObjectId`.
    fn overlap_aabb(&self, aabb: &Aabb) -> Vec<ObjectId>;
}

/// Пересечение двух коробок с положительным объёмом.
pub fn aabb_overlap(a: &Aabb, b: &Aabb) -> bool {
    a.min.x < b.max.x
        && a.max.x > b.min.x
        && a.min.y < b.max.y
        && a.max.y > b.min.y
        && a.min.z < b.max.z
        && a.max.z > b.min.z
}

/// Луч против коробки (метод плит). Возвращает расстояние и нормаль входа.
pub fn ray_aabb(origin: Vec3, dir: Vec3, b: &Aabb) -> Option<(f32, Vec3)> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    let mut normal = Vec3::ZERO;
    for axis in 0..3 {
        let (o, d, lo, hi) = (origin[axis], dir[axis], b.min[axis], b.max[axis]);
        if d.abs() < 1e-9 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = ((lo - o) / d, (hi - o) / d);
        let mut n = -1.0;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
            n = 1.0;
        }
        if t1 > t_min {
            t_min = t1;
            normal = Vec3::ZERO;
            normal[axis] = n;
        }
        t_max = t_max.min(t2);
        if t_min > t_max {
            return None;
        }
    }
    if t_max < 0.0 {
        return None;
    }
    Some(if t_min >= 0.0 {
        (t_min, normal)
    } else {
        (0.0, Vec3::ZERO)
    })
}

/// Статическая и динамическая коллизия мира в виде коробок.
#[derive(Debug, Default, Clone)]
pub struct BoxWorld {
    chunk_size_m: f32,
    statics: BTreeMap<(ChunkCoord, FloorId), Vec<(ObjectId, Aabb)>>,
    dynamics: BTreeMap<ObjectId, Aabb>,
    /// Переопределённые слои объектов (по умолчанию: статика — `LAYER_STATIC`, динамика — `LAYER_DYNAMIC`).
    layers: BTreeMap<ObjectId, u32>,
}

impl BoxWorld {
    /// Построить коллизию всего мира.
    pub fn from_world(world: &World) -> Self {
        let mut bw = Self {
            chunk_size_m: world.config().chunk_size_m,
            ..Self::default()
        };
        let floors: Vec<FloorId> = world.floors().map(|f| f.id).collect();
        for floor in floors {
            for chunk in world.populated_chunks(floor) {
                bw.rebuild_chunk(world, chunk, floor);
            }
        }
        bw
    }

    /// Пересобрать указанные регионы (результат `World::take_dirty`).
    pub fn rebuild_regions(&mut self, world: &World, regions: &[DirtyRegion]) {
        let _span = tracing::debug_span!("collision_rebuild", regions = regions.len()).entered();
        for r in regions {
            self.rebuild_chunk(world, r.chunk, r.floor);
        }
    }

    fn rebuild_chunk(&mut self, world: &World, chunk: ChunkCoord, floor: FloorId) {
        let Some(def) = world.floor(floor) else {
            self.statics.remove(&(chunk, floor));
            return;
        };
        let cfg = world.config();
        let mut boxes = Vec::new();
        for e in world.elements_in_chunk(chunk, floor) {
            // Коллизию эмитирует чанк-владелец (максимальная ячейка элемента).
            let owner = e
                .placement
                .touched_cells()
                .into_iter()
                .max()
                .map(|c| c.chunk(cfg.grid_step_m, cfg.chunk_size_m));
            if owner == Some(chunk) {
                boxes.extend(
                    collision_boxes(world, def, e)
                        .into_iter()
                        .map(|b| (e.id, b)),
                );
            }
        }
        if boxes.is_empty() {
            self.statics.remove(&(chunk, floor));
        } else {
            self.statics.insert((chunk, floor), boxes);
        }
    }

    /// Поставить или убрать динамическую коробку (закрытая дверь и т. п.).
    pub fn set_dynamic(&mut self, id: ObjectId, aabb: Option<Aabb>) {
        match aabb {
            Some(b) => {
                self.dynamics.insert(id, b);
            }
            None => {
                self.dynamics.remove(&id);
            }
        }
    }

    /// Привести коллизию дверей в соответствие с состоянием: закрытая дверь — преграда.
    pub fn sync_doors(&mut self, world: &World, is_open: impl Fn(ObjectId) -> bool) {
        let door_ids: BTreeSet<ObjectId> = world
            .elements()
            .filter(|e| matches!(e.kind, StructureKind::Opening(OpeningKind::Door)))
            .map(|e| e.id)
            .collect();
        self.dynamics.retain(|id, _| door_ids.contains(id));
        for id in door_ids {
            let bounds = world.element(id).and_then(|e| opening_bounds(world, e));
            self.set_dynamic(id, if is_open(id) { None } else { bounds });
        }
    }

    pub fn static_box_count(&self) -> usize {
        self.statics.values().map(Vec::len).sum()
    }

    pub fn dynamic_box_count(&self) -> usize {
        self.dynamics.len()
    }

    fn chunk_range(&self, aabb: &Aabb) -> (ChunkCoord, ChunkCoord) {
        if self.chunk_size_m <= 0.0 {
            // Пустой мир без размера чанка: диапазон пуст.
            return (ChunkCoord::new(1, 1), ChunkCoord::new(0, 0));
        }
        // +1 чанк с каждой стороны: коробка элемента может выходить за границу чанка-владельца.
        let lo = ChunkCoord::from_world(aabb.min, self.chunk_size_m);
        let hi = ChunkCoord::from_world(aabb.max, self.chunk_size_m);
        (
            ChunkCoord::new(lo.x - 1, lo.z - 1),
            ChunkCoord::new(hi.x + 1, hi.z + 1),
        )
    }

    /// Статические коробки чанков, потенциально пересекающих область.
    fn static_candidates(&self, aabb: &Aabb) -> impl Iterator<Item = &(ObjectId, Aabb)> + '_ {
        let (lo, hi) = self.chunk_range(aabb);
        self.statics
            .iter()
            .filter(move |((c, _), _)| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z)
            .flat_map(|(_, v)| v.iter())
    }

    /// Все коробки области, включая динамические. Порядок детерминирован.
    pub fn boxes_in(&self, aabb: &Aabb) -> Vec<(ObjectId, Aabb)> {
        let mut out: Vec<(ObjectId, Aabb)> = self
            .static_candidates(aabb)
            .filter(|(_, b)| aabb_overlap(aabb, b))
            .copied()
            .collect();
        out.extend(
            self.dynamics
                .iter()
                .filter(|(_, b)| aabb_overlap(aabb, b))
                .map(|(id, b)| (*id, *b)),
        );
        out
    }
}

impl SpatialQuery for BoxWorld {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<RayHit> {
        let dir = dir.try_normalize()?;
        let end = origin + dir * max_distance;
        let area = Aabb::new(origin.min(end), origin.max(end));
        let mut best: Option<RayHit> = None;
        let mut consider = |id: ObjectId, b: &Aabb| {
            if let Some((t, normal)) = ray_aabb(origin, dir, b)
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
        };
        let (lo, hi) = self.chunk_range(&area);
        for ((c, _), boxes) in &self.statics {
            if c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z {
                for (id, b) in boxes {
                    consider(*id, b);
                }
            }
        }
        for (id, b) in &self.dynamics {
            consider(*id, b);
        }
        best
    }

    fn overlap_aabb(&self, aabb: &Aabb) -> Vec<ObjectId> {
        let ids: BTreeSet<ObjectId> = self.boxes_in(aabb).into_iter().map(|(id, _)| id).collect();
        ids.into_iter().collect()
    }
}

/// Триггерные объёмы: события входа и выхода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEvent {
    Entered { trigger: ObjectId, actor: ObjectId },
    Exited { trigger: ObjectId, actor: ObjectId },
}

#[derive(Debug, Default)]
pub struct TriggerSet {
    volumes: BTreeMap<ObjectId, Aabb>,
    inside: HashMap<ObjectId, BTreeSet<ObjectId>>,
}

impl TriggerSet {
    pub fn insert(&mut self, id: ObjectId, volume: Aabb) {
        self.volumes.insert(id, volume);
    }

    pub fn remove(&mut self, id: ObjectId) {
        self.volumes.remove(&id);
        for set in self.inside.values_mut() {
            set.remove(&id);
        }
    }

    /// Обновить положение актора; вернуть события входа/выхода (по возрастанию `ObjectId` триггера).
    pub fn update(&mut self, actor: ObjectId, actor_box: &Aabb) -> Vec<TriggerEvent> {
        let now: BTreeSet<ObjectId> = self
            .volumes
            .iter()
            .filter(|(_, v)| aabb_overlap(v, actor_box))
            .map(|(id, _)| *id)
            .collect();
        let was = self.inside.entry(actor).or_default();
        let mut events = Vec::new();
        events.extend(
            now.difference(was)
                .map(|&t| TriggerEvent::Entered { trigger: t, actor }),
        );
        events.extend(
            was.difference(&now)
                .map(|&t| TriggerEvent::Exited { trigger: t, actor }),
        );
        *was = now;
        events.sort_by_key(|e| match e {
            TriggerEvent::Entered { trigger, .. } | TriggerEvent::Exited { trigger, .. } => {
                *trigger
            }
        });
        events
    }

    /// Актор исчез из мира: сгенерировать выходы.
    pub fn forget(&mut self, actor: ObjectId) -> Vec<TriggerEvent> {
        self.inside
            .remove(&actor)
            .into_iter()
            .flatten()
            .map(|trigger| TriggerEvent::Exited { trigger, actor })
            .collect()
    }
}

#[cfg(test)]
mod tests;
