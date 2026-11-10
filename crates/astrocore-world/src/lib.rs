//! Каноническая модель мира (раздел 8 дизайн-документа, AC-008).
//!
//! Это единственный источник истины о структуре мира. Рендер, физика,
//! навигация и топология — **производные представления**: они читают этот
//! крейт и пересобираются по dirty-регионам. Крейт не зависит ни от Bevy, ни от
//! GPU (запрещённая связь `world → render`).
//!
//! Любое изменение проходит через [`WorldCommand`]: команда валидируется,
//! фиксируется, увеличивает ревизию мира и порождает [`WorldOp`] — запись
//! журнала, которую можно повторно применить (сохранение) или переслать
//! клиентам (репликация).

mod element;
mod error;
mod state;

pub use element::{
    FloorDef, FloorId, OpeningKind, Placement, Slot, SlotKey, StructureElement, StructureKind,
};
pub use error::WorldError;
pub use state::{WORLD_SCHEMA_VERSION, WorldState};

use std::collections::{BTreeMap, BTreeSet, HashMap};

use astrocore_core::{ObjectId, Revision};
use astrocore_math::{ChunkCoord, GridCell, GridEdge};
use serde::{Deserialize, Serialize};

/// Настройки пространственной структуры мира.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldConfig {
    /// Размер чанка по горизонтали, метры. Должен быть кратен шагу сетки.
    pub chunk_size_m: f32,
    /// Шаг структурной сетки, метры.
    pub grid_step_m: f32,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            chunk_size_m: astrocore_math::DEFAULT_CHUNK_SIZE_M,
            grid_step_m: astrocore_math::DEFAULT_GRID_STEP_M,
        }
    }
}

impl WorldConfig {
    pub fn validate(&self) -> Result<(), WorldError> {
        let ok = self.chunk_size_m.is_finite()
            && self.grid_step_m.is_finite()
            && self.grid_step_m > 0.0
            && self.chunk_size_m >= self.grid_step_m;
        if !ok {
            return Err(WorldError::InvalidConfig(
                "размеры должны быть конечными и положительными".into(),
            ));
        }
        let ratio = self.chunk_size_m / self.grid_step_m;
        if (ratio - ratio.round()).abs() > 1e-4 {
            return Err(WorldError::InvalidConfig(
                "размер чанка должен быть кратен шагу сетки".into(),
            ));
        }
        Ok(())
    }
}

/// Что именно нужно пересобрать в затронутом регионе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DirtyFlags(u8);

impl DirtyFlags {
    pub const NONE: DirtyFlags = DirtyFlags(0);
    pub const GEOMETRY: DirtyFlags = DirtyFlags(1);
    pub const COLLISION: DirtyFlags = DirtyFlags(2);
    pub const NAVIGATION: DirtyFlags = DirtyFlags(4);
    pub const TOPOLOGY: DirtyFlags = DirtyFlags(8);
    pub const ALL: DirtyFlags = DirtyFlags(15);

    pub const fn union(self, other: DirtyFlags) -> DirtyFlags {
        DirtyFlags(self.0 | other.0)
    }

    pub const fn contains(self, other: DirtyFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Область, которую нужно пересобрать (чанк × этаж).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyRegion {
    pub chunk: ChunkCoord,
    pub floor: FloorId,
    pub flags: DirtyFlags,
}

/// Изменение мира, прошедшее валидацию.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorldCommand {
    DefineFloor(FloorDef),
    PlaceElement(StructureElement),
    RemoveElement { id: ObjectId },
}

/// Подтверждённая операция — запись журнала и единица репликации.
///
/// `seq` равен ревизии мира **после** применения операции, поэтому
/// последовательность операций непрерывна, а повторное применение
/// распознаётся (идемпотентность).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldOp {
    pub seq: u64,
    pub command: WorldCommand,
}

/// Результат применения реплицированной операции на клиенте/при восстановлении.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplicaOutcome {
    Applied,
    /// Операция уже учтена: повторное применение ничего не меняет.
    Duplicate,
    /// Пропущены операции — нужен базовый snapshot.
    Gap {
        expected: u64,
        got: u64,
    },
    /// Операция противоречит локальному состоянию — состояние расходится.
    Rejected(WorldError),
}

/// Каноническое состояние одного мира.
#[derive(Debug, Clone)]
pub struct World {
    id: String,
    config: WorldConfig,
    floors: BTreeMap<FloorId, FloorDef>,
    elements: BTreeMap<ObjectId, StructureElement>,
    revision: Revision,
    chunk_revisions: BTreeMap<ChunkCoord, Revision>,
    // Производные индексы (не сериализуются, перестраиваются при загрузке).
    slots: HashMap<Slot, ObjectId>,
    chunk_index: BTreeMap<(ChunkCoord, FloorId), BTreeSet<ObjectId>>,
    // Накопленные, ещё не обработанные потребителями регионы.
    dirty: BTreeMap<(ChunkCoord, FloorId), DirtyFlags>,
}

impl World {
    pub fn new(id: impl Into<String>, config: WorldConfig) -> Result<Self, WorldError> {
        config.validate()?;
        Ok(Self {
            id: id.into(),
            config,
            floors: BTreeMap::new(),
            elements: BTreeMap::new(),
            revision: Revision::ZERO,
            chunk_revisions: BTreeMap::new(),
            slots: HashMap::new(),
            chunk_index: BTreeMap::new(),
            dirty: BTreeMap::new(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn config(&self) -> &WorldConfig {
        &self.config
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn chunk_revision(&self, chunk: ChunkCoord) -> Revision {
        self.chunk_revisions
            .get(&chunk)
            .copied()
            .unwrap_or(Revision::ZERO)
    }

    pub fn floor(&self, id: FloorId) -> Option<&FloorDef> {
        self.floors.get(&id)
    }

    pub fn floors(&self) -> impl Iterator<Item = &FloorDef> {
        self.floors.values()
    }

    pub fn element(&self, id: ObjectId) -> Option<&StructureElement> {
        self.elements.get(&id)
    }

    /// Все элементы в порядке возрастания `ObjectId` (детерминированно).
    pub fn elements(&self) -> impl Iterator<Item = &StructureElement> {
        self.elements.values()
    }

    pub fn element_count(&self) -> usize {
        self.elements.len()
    }

    pub fn element_at(&self, floor: FloorId, key: SlotKey) -> Option<&StructureElement> {
        self.slots
            .get(&Slot { floor, key })
            .and_then(|id| self.elements.get(id))
    }

    /// Элемент, занимающий ребро (стена или проём).
    pub fn edge_element(&self, floor: FloorId, edge: GridEdge) -> Option<&StructureElement> {
        self.element_at(floor, SlotKey::Edge(edge))
    }

    /// Элементы чанка на этаже, в порядке `ObjectId`.
    pub fn elements_in_chunk(
        &self,
        chunk: ChunkCoord,
        floor: FloorId,
    ) -> impl Iterator<Item = &StructureElement> {
        self.chunk_index
            .get(&(chunk, floor))
            .into_iter()
            .flatten()
            .filter_map(|id| self.elements.get(id))
    }

    /// Чанки, в которых есть элементы этажа.
    pub fn populated_chunks(&self, floor: FloorId) -> Vec<ChunkCoord> {
        self.chunk_index
            .keys()
            .filter(|(_, f)| *f == floor)
            .map(|(c, _)| *c)
            .collect()
    }

    /// Ограничивающий прямоугольник ячеек, затронутых элементами этажа.
    pub fn floor_cell_bounds(&self, floor: FloorId) -> Option<(GridCell, GridCell)> {
        let mut bounds: Option<(GridCell, GridCell)> = None;
        for e in self.elements.values().filter(|e| e.floor == floor) {
            for c in e.placement.touched_cells() {
                bounds = Some(match bounds {
                    None => (c, c),
                    Some((lo, hi)) => (
                        GridCell::new(lo.x.min(c.x), lo.z.min(c.z)),
                        GridCell::new(hi.x.max(c.x), hi.z.max(c.z)),
                    ),
                });
            }
        }
        bounds
    }

    /// Чанки, чья геометрия зависит от элемента.
    pub fn affected_chunks(&self, e: &StructureElement) -> BTreeSet<ChunkCoord> {
        e.placement
            .touched_cells()
            .into_iter()
            .map(|c| c.chunk(self.config.grid_step_m, self.config.chunk_size_m))
            .collect()
    }

    /// Применить команду. Возвращает подтверждённую операцию для журнала/сети.
    pub fn apply(&mut self, command: WorldCommand) -> Result<WorldOp, WorldError> {
        self.commit(&command)?;
        Ok(WorldOp {
            seq: self.revision.0,
            command,
        })
    }

    /// То же, но с оптимистичной блокировкой: команда отклоняется, если мир
    /// изменился с момента, когда автор команды видел ревизию `expected`.
    pub fn apply_checked(
        &mut self,
        command: WorldCommand,
        expected: Revision,
    ) -> Result<WorldOp, WorldError> {
        if self.revision != expected {
            return Err(WorldError::StaleRevision {
                expected,
                actual: self.revision,
            });
        }
        self.apply(command)
    }

    /// Применить операцию из журнала или от сервера (идемпотентно, по порядку).
    pub fn apply_replicated(&mut self, op: &WorldOp) -> ReplicaOutcome {
        let current = self.revision.0;
        if op.seq <= current {
            return ReplicaOutcome::Duplicate;
        }
        if op.seq != current + 1 {
            return ReplicaOutcome::Gap {
                expected: current + 1,
                got: op.seq,
            };
        }
        match self.commit(&op.command) {
            Ok(()) => ReplicaOutcome::Applied,
            Err(e) => ReplicaOutcome::Rejected(e),
        }
    }

    /// Забрать накопленные dirty-регионы (потребитель пересобирает производные данные).
    pub fn take_dirty(&mut self) -> Vec<DirtyRegion> {
        std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|((chunk, floor), flags)| DirtyRegion {
                chunk,
                floor,
                flags,
            })
            .collect()
    }

    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    fn commit(&mut self, command: &WorldCommand) -> Result<(), WorldError> {
        // Сначала вся валидация, потом мутация: при ошибке мир не меняется.
        match command {
            WorldCommand::DefineFloor(def) => {
                def.validate()?;
                if self.floors.contains_key(&def.id) {
                    return Err(WorldError::DuplicateFloor(def.id));
                }
                self.floors.insert(def.id, def.clone());
                self.revision = self.revision.next();
            }
            WorldCommand::PlaceElement(element) => {
                self.validate_element(element)?;
                let slot = element.slot();
                if let Some(&by) = self.slots.get(&slot) {
                    return Err(WorldError::SlotOccupied { slot, by });
                }
                self.revision = self.revision.next();
                self.index_insert(element);
                self.mark_dirty(element);
                self.elements.insert(element.id, element.clone());
            }
            WorldCommand::RemoveElement { id } => {
                let element = self
                    .elements
                    .get(id)
                    .cloned()
                    .ok_or(WorldError::UnknownElement(*id))?;
                self.revision = self.revision.next();
                self.index_remove(&element);
                self.mark_dirty(&element);
                self.elements.remove(id);
            }
        }
        Ok(())
    }

    fn validate_element(&self, e: &StructureElement) -> Result<(), WorldError> {
        e.validate(self.config.grid_step_m)?;
        if !self.floors.contains_key(&e.floor) {
            return Err(WorldError::UnknownFloor(e.floor));
        }
        if self.elements.contains_key(&e.id) {
            return Err(WorldError::DuplicateId(e.id));
        }
        Ok(())
    }

    fn index_insert(&mut self, e: &StructureElement) {
        self.slots.insert(e.slot(), e.id);
        for chunk in self.affected_chunks(e) {
            self.chunk_index
                .entry((chunk, e.floor))
                .or_default()
                .insert(e.id);
        }
    }

    fn index_remove(&mut self, e: &StructureElement) {
        self.slots.remove(&e.slot());
        for chunk in self.affected_chunks(e) {
            if let Some(set) = self.chunk_index.get_mut(&(chunk, e.floor)) {
                set.remove(&e.id);
                if set.is_empty() {
                    self.chunk_index.remove(&(chunk, e.floor));
                }
            }
        }
    }

    fn mark_dirty(&mut self, e: &StructureElement) {
        let flags = e.kind.dirty_flags();
        let rev = self.revision;
        for chunk in self.affected_chunks(e) {
            let entry = self.dirty.entry((chunk, e.floor)).or_default();
            *entry = entry.union(flags);
            self.chunk_revisions.insert(chunk, rev);
        }
    }
}

#[cfg(test)]
mod tests;
