use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{GridCell, GridEdge};
use serde::{Deserialize, Serialize};

use crate::{DirtyFlags, WorldError};

/// Этаж — редакторская группировка с диапазоном высот, а не отдельная физическая плоскость.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FloorId(pub i32);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloorDef {
    pub id: FloorId,
    pub name: String,
    /// Высота низа этажа (ось Y), метры.
    pub base_y: f32,
    /// Высота этажа, метры. Не обязана совпадать с шагом сетки.
    pub height: f32,
}

impl FloorDef {
    pub fn validate(&self) -> Result<(), WorldError> {
        if !self.base_y.is_finite() || !self.height.is_finite() || self.height <= 0.0 {
            return Err(WorldError::InvalidFloor(format!(
                "base_y={} height={}: ожидаются конечные числа и height > 0",
                self.base_y, self.height
            )));
        }
        Ok(())
    }

    pub fn top_y(&self) -> f32 {
        self.base_y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OpeningKind {
    Door,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StructureKind {
    /// Сплошная стена на ребре сетки.
    Wall,
    /// Проём на ребре сетки (вместо стены). Дверь как runtime-сущность привязывается к нему.
    Opening(OpeningKind),
    /// Перекрытие ячейки.
    Floor,
    Ceiling,
}

impl StructureKind {
    pub(crate) fn dirty_flags(self) -> DirtyFlags {
        match self {
            StructureKind::Wall | StructureKind::Opening(_) => DirtyFlags::ALL,
            StructureKind::Floor => DirtyFlags::GEOMETRY
                .union(DirtyFlags::COLLISION)
                .union(DirtyFlags::NAVIGATION),
            StructureKind::Ceiling => DirtyFlags::GEOMETRY.union(DirtyFlags::COLLISION),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Placement {
    Edge(GridEdge),
    Cell(GridCell),
}

impl Placement {
    /// Ячейки, чья геометрия зависит от элемента.
    pub fn touched_cells(self) -> Vec<GridCell> {
        match self {
            Placement::Edge(e) => e.adjacent_cells().to_vec(),
            Placement::Cell(c) => vec![c],
        }
    }
}

/// Ключ занимаемого места внутри этажа: в одном месте — один элемент.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SlotKey {
    /// Стена или проём.
    Edge(GridEdge),
    FloorSlab(GridCell),
    CeilingSlab(GridCell),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Slot {
    pub floor: FloorId,
    pub key: SlotKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructureElement {
    pub id: ObjectId,
    pub kind: StructureKind,
    pub placement: Placement,
    pub floor: FloorId,
    pub material: PrototypeId,
    /// Толщина, метры (для перекрытий — толщина плиты).
    pub thickness: f32,
}

impl StructureElement {
    pub fn slot(&self) -> Slot {
        let key = match (self.kind, self.placement) {
            (StructureKind::Floor, Placement::Cell(c)) => SlotKey::FloorSlab(c),
            (StructureKind::Ceiling, Placement::Cell(c)) => SlotKey::CeilingSlab(c),
            (_, Placement::Edge(e)) => SlotKey::Edge(e),
            // Недопустимые сочетания отсекает validate(); здесь возвращаем что-то стабильное.
            (_, Placement::Cell(c)) => SlotKey::FloorSlab(c),
        };
        Slot {
            floor: self.floor,
            key,
        }
    }

    pub fn validate(&self, grid_step_m: f32) -> Result<(), WorldError> {
        let placement_ok = matches!(
            (self.kind, self.placement),
            (
                StructureKind::Wall | StructureKind::Opening(_),
                Placement::Edge(_)
            ) | (
                StructureKind::Floor | StructureKind::Ceiling,
                Placement::Cell(_)
            )
        );
        if !placement_ok {
            return Err(WorldError::InvalidPlacement(format!(
                "{:?} нельзя разместить как {:?}",
                self.kind, self.placement
            )));
        }
        if !self.thickness.is_finite() || self.thickness <= 0.0 || self.thickness > grid_step_m {
            return Err(WorldError::InvalidDimension(format!(
                "thickness={}: ожидается 0 < thickness <= {grid_step_m}",
                self.thickness
            )));
        }
        Ok(())
    }
}
