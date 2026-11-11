//! Рельеф открытых территорий (раздел 9.2 дизайн-документа): карта высот в вершинах
//! регулярной сетки, кисти правки, дороги, уровень воды, меши и запрос высоты земли.
//!
//! Правки — это [`TerrainEdit`] (набор «вершина → новая высота»). Они обратимы
//! (`TerrainEdit::inverse` хранит прежние значения), нумеруются ревизиями
//! ([`TerrainOp`]) и поэтому подходят и для undo/redo редактора, и для журнала, и для
//! репликации — по тем же правилам, что и операции структурного мира.
//!
//! Треугольники каждой ячейки режутся по фиксированной диагонали, и высота
//! [`Terrain::sample`] считается по тем же треугольникам, что рисуются и используются
//! в коллизии, — поверхность для рендера, физики и навигации одна.

use std::collections::{BTreeMap, BTreeSet};

use astrocore_geometry::MeshData;
use astrocore_math::ChunkCoord;
use astrocore_math::glam::{Vec2, Vec3};
use astrocore_physics::GroundProvider;
use serde::{Deserialize, Serialize};

/// Верхняя граница высоты вершины, метры (защита от мусора из файла и сети).
pub const MAX_HEIGHT_M: f32 = 10_000.0;
/// Максимум вершин в одной правке.
pub const MAX_EDIT_VERTICES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TerrainError {
    #[error("неверные параметры рельефа: {0}")]
    Config(String),
    #[error("высота {0} вне допустимого диапазона")]
    BadHeight(f32),
    #[error("слишком большая правка ({0} вершин)")]
    TooLarge(usize),
    #[error(
        "правка не соответствует состоянию: вершина ({x}, {z}) имеет высоту {actual}, ожидалась {expected}"
    )]
    Stale {
        x: i32,
        z: i32,
        expected: f32,
        actual: f32,
    },
    #[error("пропуск ревизий: ожидалась {expected}, получена {got}")]
    Gap { expected: u64, got: u64 },
    #[error("повреждённое состояние рельефа: {0}")]
    Corrupt(String),
}

/// Вершина карты высот.
pub type Vertex = (i32, i32);

/// Правка: список «вершина, прежняя высота, новая высота».
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TerrainEdit {
    pub changes: Vec<(i32, i32, f32, f32)>,
}

impl TerrainEdit {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Обратная правка (для undo).
    #[must_use]
    pub fn inverse(&self) -> TerrainEdit {
        TerrainEdit {
            changes: self
                .changes
                .iter()
                .rev()
                .map(|&(x, z, old, new)| (x, z, new, old))
                .collect(),
        }
    }
}

/// Подтверждённая правка с номером ревизии рельефа после применения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainOp {
    pub seq: u64,
    pub edit: TerrainEdit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainState {
    pub resolution_m: f32,
    pub chunk_cells: u32,
    pub base_height: f32,
    pub water_level: Option<f32>,
    pub revision: u64,
    /// Вершины, отличающиеся от базовой высоты, по возрастанию `(x, z)`.
    pub vertices: Vec<(i32, i32, f32)>,
}

#[derive(Debug, Clone)]
pub struct Terrain {
    resolution_m: f32,
    chunk_cells: u32,
    base_height: f32,
    water_level: Option<f32>,
    heights: BTreeMap<Vertex, f32>,
    revision: u64,
    dirty: BTreeSet<ChunkCoord>,
}

fn div_floor(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

impl Terrain {
    /// `resolution_m` — расстояние между вершинами; `chunk_cells` — ячеек в чанке по стороне.
    pub fn new(
        resolution_m: f32,
        chunk_cells: u32,
        base_height: f32,
    ) -> Result<Self, TerrainError> {
        if !(resolution_m.is_finite() && resolution_m > 0.0) {
            return Err(TerrainError::Config(
                "шаг сетки должен быть положительным".into(),
            ));
        }
        if chunk_cells == 0 || chunk_cells > 1024 {
            return Err(TerrainError::Config("размер чанка: 1..=1024 ячеек".into()));
        }
        if !base_height.is_finite() || base_height.abs() > MAX_HEIGHT_M {
            return Err(TerrainError::BadHeight(base_height));
        }
        Ok(Self {
            resolution_m,
            chunk_cells,
            base_height,
            water_level: None,
            heights: BTreeMap::new(),
            revision: 0,
            dirty: BTreeSet::new(),
        })
    }

    pub fn resolution(&self) -> f32 {
        self.resolution_m
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn water_level(&self) -> Option<f32> {
        self.water_level
    }

    pub fn set_water_level(&mut self, level: Option<f32>) {
        self.water_level = level.filter(|l| l.is_finite());
    }

    pub fn height_at_vertex(&self, x: i32, z: i32) -> f32 {
        self.heights
            .get(&(x, z))
            .copied()
            .unwrap_or(self.base_height)
    }

    fn chunk_of_vertex(&self, x: i32, z: i32) -> ChunkCoord {
        let n = self.chunk_cells as i32;
        ChunkCoord::new(div_floor(x, n), div_floor(z, n))
    }

    /// Чанки, чьи треугольники зависят от вершины (вершина на границе — до четырёх чанков).
    fn chunks_touching(&self, x: i32, z: i32) -> BTreeSet<ChunkCoord> {
        let mut out = BTreeSet::new();
        for (dx, dz) in [(0, 0), (-1, 0), (0, -1), (-1, -1)] {
            out.insert(self.chunk_of_vertex(x + dx, z + dz));
        }
        out
    }

    /// Применить правку, проверив, что прежние значения совпадают с текущими.
    pub fn apply(&mut self, edit: &TerrainEdit) -> Result<TerrainOp, TerrainError> {
        if edit.changes.len() > MAX_EDIT_VERTICES {
            return Err(TerrainError::TooLarge(edit.changes.len()));
        }
        // Сначала проверка целиком, потом изменение: ошибка ничего не портит.
        let mut shadow: BTreeMap<Vertex, f32> = BTreeMap::new();
        for &(x, z, old, new) in &edit.changes {
            if !new.is_finite() || new.abs() > MAX_HEIGHT_M {
                return Err(TerrainError::BadHeight(new));
            }
            let current = shadow
                .get(&(x, z))
                .copied()
                .unwrap_or_else(|| self.height_at_vertex(x, z));
            if (current - old).abs() > 1e-4 {
                return Err(TerrainError::Stale {
                    x,
                    z,
                    expected: old,
                    actual: current,
                });
            }
            shadow.insert((x, z), new);
        }
        for (&(x, z), &h) in &shadow {
            if (h - self.base_height).abs() < 1e-6 {
                self.heights.remove(&(x, z));
            } else {
                self.heights.insert((x, z), h);
            }
            let touched = self.chunks_touching(x, z);
            self.dirty.extend(touched);
        }
        self.revision += 1;
        Ok(TerrainOp {
            seq: self.revision,
            edit: edit.clone(),
        })
    }

    /// Идемпотентное применение операции из журнала или от сервера.
    pub fn apply_replicated(&mut self, op: &TerrainOp) -> Result<bool, TerrainError> {
        if op.seq <= self.revision {
            return Ok(false);
        }
        if op.seq != self.revision + 1 {
            return Err(TerrainError::Gap {
                expected: self.revision + 1,
                got: op.seq,
            });
        }
        self.apply(&op.edit)?;
        Ok(true)
    }

    pub fn take_dirty(&mut self) -> Vec<ChunkCoord> {
        std::mem::take(&mut self.dirty).into_iter().collect()
    }

    // ---- высота и склон -------------------------------------------------------

    /// Высота поверхности в точке (x, z), метры. Бесконечный рельеф: вне изменённой области — базовая высота.
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        let (gx, gz) = (x / self.resolution_m, z / self.resolution_m);
        let (ix, iz) = (gx.floor() as i32, gz.floor() as i32);
        let (u, v) = (gx - ix as f32, gz - iz as f32);
        let h = |dx, dz| self.height_at_vertex(ix + dx, iz + dz);
        // Ячейка режется диагональю (0,0)–(1,1): два треугольника.
        if u >= v {
            let (h00, h10, h11) = (h(0, 0), h(1, 0), h(1, 1));
            h00 + (h10 - h00) * u + (h11 - h10) * v
        } else {
            let (h00, h01, h11) = (h(0, 0), h(0, 1), h(1, 1));
            h00 + (h11 - h01) * u + (h01 - h00) * v
        }
    }

    /// Нормаль поверхности (единичная, направлена вверх).
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        let e = self.resolution_m * 0.5;
        let dx = self.sample(x + e, z) - self.sample(x - e, z);
        let dz = self.sample(x, z + e) - self.sample(x, z - e);
        Vec3::new(-dx / (2.0 * e), 1.0, -dz / (2.0 * e)).normalize()
    }

    /// Угол наклона в градусах.
    pub fn slope_deg(&self, x: f32, z: f32) -> f32 {
        self.normal(x, z).y.clamp(-1.0, 1.0).acos().to_degrees()
    }

    pub fn is_walkable(&self, x: f32, z: f32, max_slope_deg: f32) -> bool {
        self.slope_deg(x, z) <= max_slope_deg
            && self
                .water_level
                .is_none_or(|w| self.sample(x, z) >= w - 0.3)
    }

    pub fn is_submerged(&self, pos: Vec3) -> bool {
        self.water_level
            .is_some_and(|w| pos.y < w && self.sample(pos.x, pos.z) < w)
    }

    // ---- кисти ----------------------------------------------------------------

    fn vertices_in_radius(&self, center: Vec2, radius: f32) -> Vec<(i32, i32, f32)> {
        let r_cells = (radius / self.resolution_m).ceil() as i32 + 1;
        let (cx, cz) = (
            (center.x / self.resolution_m).round() as i32,
            (center.y / self.resolution_m).round() as i32,
        );
        let mut out = Vec::new();
        for x in (cx - r_cells)..=(cx + r_cells) {
            for z in (cz - r_cells)..=(cz + r_cells) {
                let d = Vec2::new(x as f32 * self.resolution_m, z as f32 * self.resolution_m)
                    .distance(center);
                if d <= radius {
                    out.push((x, z, d));
                }
            }
        }
        out
    }

    fn edit_from(&self, targets: impl IntoIterator<Item = (i32, i32, f32)>) -> TerrainEdit {
        TerrainEdit {
            changes: targets
                .into_iter()
                .filter_map(|(x, z, new)| {
                    let old = self.height_at_vertex(x, z);
                    ((new - old).abs() > 1e-5).then_some((
                        x,
                        z,
                        old,
                        new.clamp(-MAX_HEIGHT_M, MAX_HEIGHT_M),
                    ))
                })
                .collect(),
        }
    }

    /// Поднять (`amount` > 0) или опустить (< 0) рельеф в круге с плавным спадом к краю.
    pub fn brush_raise(&self, center: Vec2, radius: f32, amount: f32) -> TerrainEdit {
        self.edit_from(
            self.vertices_in_radius(center, radius)
                .into_iter()
                .map(|(x, z, d)| {
                    let w = smoothstep(1.0 - d / radius);
                    (x, z, self.height_at_vertex(x, z) + amount * w)
                }),
        )
    }

    /// Сгладить: каждая вершина тянется к среднему соседей.
    pub fn brush_smooth(&self, center: Vec2, radius: f32, strength: f32) -> TerrainEdit {
        let s = strength.clamp(0.0, 1.0);
        self.edit_from(
            self.vertices_in_radius(center, radius)
                .into_iter()
                .map(|(x, z, d)| {
                    let avg = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .map(|(dx, dz)| self.height_at_vertex(x + dx, z + dz))
                        .sum::<f32>()
                        / 4.0;
                    let w = smoothstep(1.0 - d / radius) * s;
                    let h = self.height_at_vertex(x, z);
                    (x, z, h + (avg - h) * w)
                }),
        )
    }

    /// Выровнять до высоты `target` в круге (с плавным спадом).
    pub fn brush_flatten(&self, center: Vec2, radius: f32, target: f32) -> TerrainEdit {
        self.edit_from(
            self.vertices_in_radius(center, radius)
                .into_iter()
                .map(|(x, z, d)| {
                    let w = smoothstep(1.0 - d / radius);
                    let h = self.height_at_vertex(x, z);
                    (x, z, h + (target - h) * w)
                }),
        )
    }

    /// Фундамент: площадка `min..max` (метры, X/Z) выравнивается до `height`, вокруг — плавный
    /// склон шириной `shoulder`. Так замок ставится на холм, а не «висит» над ним.
    pub fn flatten_rect(&self, min: Vec2, max: Vec2, height: f32, shoulder: f32) -> TerrainEdit {
        let (lo, hi) = (min.min(max), min.max(max));
        let pad = shoulder.max(0.0);
        let r = self.resolution_m;
        let (x0, x1) = (
            ((lo.x - pad) / r).floor() as i32,
            ((hi.x + pad) / r).ceil() as i32,
        );
        let (z0, z1) = (
            ((lo.y - pad) / r).floor() as i32,
            ((hi.y + pad) / r).ceil() as i32,
        );
        let mut targets = Vec::new();
        for x in x0..=x1 {
            for z in z0..=z1 {
                let p = Vec2::new(x as f32 * r, z as f32 * r);
                let outside = Vec2::new(
                    (lo.x - p.x).max(p.x - hi.x).max(0.0),
                    (lo.y - p.y).max(p.y - hi.y).max(0.0),
                )
                .length();
                let w = if outside <= 0.0 {
                    1.0
                } else if pad > 0.0 {
                    smoothstep(1.0 - outside / pad)
                } else {
                    0.0
                };
                if w > 0.0 {
                    let h = self.height_at_vertex(x, z);
                    targets.push((x, z, h + (height - h) * w));
                }
            }
        }
        self.edit_from(targets)
    }

    /// Дорога: полотно шириной `width` вдоль ломаной с высотами, интерполированными от
    /// начала к концу пути по длине; обочины шириной `shoulder` плавно сходят на нет.
    pub fn road(&self, path: &[Vec2], width: f32, shoulder: f32) -> TerrainEdit {
        if path.len() < 2 {
            return TerrainEdit::default();
        }
        let lengths: Vec<f32> = path.windows(2).map(|w| w[0].distance(w[1])).collect();
        let total: f32 = lengths.iter().sum::<f32>().max(1e-6);
        let (h_start, h_end) = (self.sample(path[0].x, path[0].y), {
            let l = path[path.len() - 1];
            self.sample(l.x, l.y)
        });
        let half = width * 0.5;
        let reach = half + shoulder.max(0.0);
        let r = self.resolution_m;
        let (mut lo, mut hi) = (path[0], path[0]);
        for p in path {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let mut targets = Vec::new();
        for x in (((lo.x - reach) / r).floor() as i32)..=(((hi.x + reach) / r).ceil() as i32) {
            for z in (((lo.y - reach) / r).floor() as i32)..=(((hi.y + reach) / r).ceil() as i32) {
                let p = Vec2::new(x as f32 * r, z as f32 * r);
                // Ближайшая точка пути и пройденная до неё доля длины.
                let mut best = (f32::INFINITY, 0.0f32);
                let mut walked = 0.0;
                for (seg, len) in path.windows(2).zip(&lengths) {
                    let (a, b) = (seg[0], seg[1]);
                    let ab = b - a;
                    let t = if *len > 0.0 {
                        ((p - a).dot(ab) / (len * len)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = p.distance(a + ab * t);
                    if d < best.0 {
                        best = (d, (walked + t * len) / total);
                    }
                    walked += len;
                }
                let (d, frac) = best;
                if d > reach {
                    continue;
                }
                let road_h = h_start + (h_end - h_start) * frac;
                let w = if d <= half {
                    1.0
                } else {
                    smoothstep(1.0 - (d - half) / shoulder.max(1e-6))
                };
                let h = self.height_at_vertex(x, z);
                targets.push((x, z, h + (road_h - h) * w));
            }
        }
        self.edit_from(targets)
    }

    // ---- меши -----------------------------------------------------------------

    /// Чанки, в которых есть изменённые вершины.
    pub fn populated_chunks(&self) -> Vec<ChunkCoord> {
        let mut set = BTreeSet::new();
        for &(x, z) in self.heights.keys() {
            set.extend(self.chunks_touching(x, z));
        }
        set.into_iter().collect()
    }

    /// Меш чанка: две треугольника на ячейку, нормали по градиенту, UV в метрах.
    pub fn build_chunk_mesh(&self, chunk: ChunkCoord) -> MeshData {
        let n = self.chunk_cells as i32;
        let r = self.resolution_m;
        let mut mesh = MeshData::default();
        let (x0, z0) = (chunk.x * n, chunk.z * n);
        let verts = (n + 1) as usize;
        for z in 0..=n {
            for x in 0..=n {
                let (vx, vz) = (x0 + x, z0 + z);
                let (wx, wz) = (vx as f32 * r, vz as f32 * r);
                let h = self.height_at_vertex(vx, vz);
                mesh.positions.push([wx, h, wz]);
                // Нормаль по центральным разностям соседних вершин (одинакова в соседних чанках — швов нет).
                let dx = self.height_at_vertex(vx + 1, vz) - self.height_at_vertex(vx - 1, vz);
                let dz = self.height_at_vertex(vx, vz + 1) - self.height_at_vertex(vx, vz - 1);
                mesh.normals.push(
                    Vec3::new(-dx / (2.0 * r), 1.0, -dz / (2.0 * r))
                        .normalize()
                        .to_array(),
                );
                mesh.uvs.push([wx, wz]);
            }
        }
        for z in 0..n as usize {
            for x in 0..n as usize {
                let i = (z * verts + x) as u32;
                let (a, b, c, d) = (i, i + 1, i + verts as u32, i + verts as u32 + 1);
                // Диагональ (0,0)–(1,1): как в `sample`. Обход — против часовой стрелки сверху.
                mesh.indices.extend_from_slice(&[a, c, d, a, d, b]);
            }
        }
        mesh
    }

    // ---- состояние ------------------------------------------------------------

    pub fn to_state(&self) -> TerrainState {
        TerrainState {
            resolution_m: self.resolution_m,
            chunk_cells: self.chunk_cells,
            base_height: self.base_height,
            water_level: self.water_level,
            revision: self.revision,
            vertices: self.heights.iter().map(|(&(x, z), &h)| (x, z, h)).collect(),
        }
    }

    pub fn from_state(state: TerrainState) -> Result<Self, TerrainError> {
        let mut t = Terrain::new(state.resolution_m, state.chunk_cells, state.base_height)?;
        if state.vertices.len() > 100_000_000 {
            return Err(TerrainError::Corrupt("слишком много вершин".into()));
        }
        for (x, z, h) in state.vertices {
            if !h.is_finite() || h.abs() > MAX_HEIGHT_M {
                return Err(TerrainError::BadHeight(h));
            }
            if t.heights.insert((x, z), h).is_some() {
                return Err(TerrainError::Corrupt(format!(
                    "вершина ({x}, {z}) повторяется"
                )));
            }
        }
        t.revision = state.revision;
        t.set_water_level(state.water_level);
        Ok(t)
    }

    /// Хеш содержимого для сверки (FNV-1a по битам высот).
    pub fn content_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        feed(&self.resolution_m.to_le_bytes());
        feed(&self.chunk_cells.to_le_bytes());
        feed(&self.base_height.to_le_bytes());
        for (&(x, z), &v) in &self.heights {
            feed(&x.to_le_bytes());
            feed(&z.to_le_bytes());
            feed(&v.to_le_bytes());
        }
        h
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl GroundProvider for Terrain {
    fn ground_height(&self, x: f32, z: f32) -> Option<f32> {
        Some(self.sample(x, z))
    }
}

#[cfg(test)]
mod tests;
