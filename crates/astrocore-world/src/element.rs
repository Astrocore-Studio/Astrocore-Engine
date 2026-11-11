use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{Facing, GridCell, GridEdge};
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
    /// Лестница: ступени от этажа элемента до этажа `to`.
    Stair {
        to: FloorId,
    },
    /// Пандус: наклонная поверхность от этажа элемента до этажа `to`.
    Ramp {
        to: FloorId,
    },
}

impl StructureKind {
    /// Этаж, куда ведёт вертикальный элемент.
    pub const fn target_floor(self) -> Option<FloorId> {
        match self {
            StructureKind::Stair { to } | StructureKind::Ramp { to } => Some(to),
            _ => None,
        }
    }

    pub(crate) fn dirty_flags(self) -> DirtyFlags {
        match self {
            StructureKind::Wall
            | StructureKind::Opening(_)
            | StructureKind::Stair { .. }
            | StructureKind::Ramp { .. } => DirtyFlags::ALL,
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
    /// Прямой марш из `length` ячеек, начиная с `start` в направлении `dir`.
    /// Входят в марш с `start`, выходят на верхнем этаже в ячейке `start + dir·length`.
    Run {
        start: GridCell,
        dir: Facing,
        length: u8,
    },
}

/// Максимальная длина марша лестницы/пандуса, ячеек.
pub const MAX_RUN_LENGTH: u8 = 32;

impl Placement {
    /// Ячейки марша (для остальных видов размещения — пусто).
    pub fn run_cells(self) -> Vec<GridCell> {
        match self {
            Placement::Run { start, dir, length } => {
                (0..i32::from(length)).map(|i| start.step(dir, i)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Ячейка выхода на верхний этаж.
    pub fn run_landing(self) -> Option<GridCell> {
        match self {
            Placement::Run { start, dir, length } => Some(start.step(dir, i32::from(length))),
            _ => None,
        }
    }
}

impl Placement {
    /// Ячейки, чья геометрия зависит от элемента.
    pub fn touched_cells(self) -> Vec<GridCell> {
        match self {
            Placement::Edge(e) => e.adjacent_cells().to_vec(),
            Placement::Cell(c) => vec![c],
            run @ Placement::Run { .. } => run.run_cells(),
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
    /// Ячейка, занятая маршем лестницы или пандуса.
    Run(GridCell),
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
    /// Все места, которые занимает элемент (у марша — по месту на ячейку).
    pub fn slots(&self) -> Vec<Slot> {
        let keys = match (self.kind, self.placement) {
            (StructureKind::Floor, Placement::Cell(c)) => vec![SlotKey::FloorSlab(c)],
            (StructureKind::Ceiling, Placement::Cell(c)) => vec![SlotKey::CeilingSlab(c)],
            (_, Placement::Edge(e)) => vec![SlotKey::Edge(e)],
            (_, run @ Placement::Run { .. }) => {
                run.run_cells().into_iter().map(SlotKey::Run).collect()
            }
            // Недопустимые сочетания отсекает validate().
            (_, Placement::Cell(c)) => vec![SlotKey::FloorSlab(c)],
        };
        keys.into_iter()
            .map(|key| Slot {
                floor: self.floor,
                key,
            })
            .collect()
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
            ) | (
                StructureKind::Stair { .. } | StructureKind::Ramp { .. },
                Placement::Run { .. }
            )
        );
        if !placement_ok {
            return Err(WorldError::InvalidPlacement(format!(
                "{:?} нельзя разместить как {:?}",
                self.kind, self.placement
            )));
        }
        if let Placement::Run { length, .. } = self.placement
            && (length == 0 || length > MAX_RUN_LENGTH)
        {
            return Err(WorldError::InvalidPlacement(format!(
                "длина марша {length}: ожидается 1..={MAX_RUN_LENGTH}"
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
