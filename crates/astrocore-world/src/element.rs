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
    /// Свободно размещённая модель вне сетки. Идентификатор модели хранится в поле
    /// `material` элемента; сама модель (GLB) живёт в ассетах клиента. Для коллизии и
    /// индексации мир хранит только габаритный ящик из [`FreePlacement`].
    Freeform,
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
            StructureKind::Ceiling | StructureKind::Freeform => {
                DirtyFlags::GEOMETRY.union(DirtyFlags::COLLISION)
            }
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
    /// Свободное размещение вне сетки.
    Free(FreePlacement),
}

/// Свободное размещение: якорная ячейка (для индексации по чанкам) и смещения внутри неё.
///
/// Всё целочисленное (миллиметры и сотые доли градуса), поэтому элемент можно хешировать,
/// сравнивать и сериализовать без неоднозначности чисел с плавающей точкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FreePlacement {
    /// Ячейка, в которой стоят «ноги» модели.
    pub cell: GridCell,
    /// Смещение внутри ячейки по X и Z, мм; обязано быть меньше шага сетки.
    pub x_mm: u32,
    pub z_mm: u32,
    /// Высота «ног» над нулём мира, мм.
    pub y_mm: i32,
    /// Поворот вокруг +Y, сотые доли градуса (положительный — против часовой стрелки сверху).
    pub yaw_cdeg: i32,
    /// Полуразмеры габаритного ящика, мм: по X, по высоте (вверх от ног) и по Z в системе модели.
    pub half_mm: [u32; 3],
}

/// Верхняя граница полуразмера габаритного ящика, мм (100 м).
pub const MAX_FREE_HALF_MM: u32 = 100_000;

impl FreePlacement {
    /// Положение «ног» в метрах.
    pub fn position(&self, step_m: f32) -> [f32; 3] {
        [
            self.cell.x as f32 * step_m + self.x_mm as f32 / 1000.0,
            self.y_mm as f32 / 1000.0,
            self.cell.z as f32 * step_m + self.z_mm as f32 / 1000.0,
        ]
    }

    pub fn yaw_rad(&self) -> f32 {
        (self.yaw_cdeg as f32 / 100.0).to_radians()
    }

    /// Осеориентированный габарит с учётом поворота: (min, max) в метрах.
    pub fn aabb(&self, step_m: f32) -> ([f32; 3], [f32; 3]) {
        let p = self.position(step_m);
        let (hx, hy, hz) = (
            self.half_mm[0] as f32 / 1000.0,
            self.half_mm[1] as f32 / 1000.0,
            self.half_mm[2] as f32 / 1000.0,
        );
        let (s, c) = self.yaw_rad().sin_cos();
        let (ex, ez) = (c.abs() * hx + s.abs() * hz, s.abs() * hx + c.abs() * hz);
        (
            [p[0] - ex, p[1], p[2] - ez],
            [p[0] + ex, p[1] + 2.0 * hy, p[2] + ez],
        )
    }

    /// Создать из непрерывной позиции (метры) — для редактора.
    pub fn from_position(pos: [f32; 3], step_m: f32, yaw_deg: f32, half_m: [f32; 3]) -> Self {
        let cell =
            GridCell::from_world(astrocore_math::glam::Vec3::new(pos[0], 0.0, pos[2]), step_m);
        let mm = |v: f32| (v * 1000.0).round() as i64;
        let (ox, oz) = (
            mm(pos[0] - cell.x as f32 * step_m),
            mm(pos[2] - cell.z as f32 * step_m),
        );
        let max_off = (step_m * 1000.0).round() as i64 - 1;
        FreePlacement {
            cell,
            x_mm: ox.clamp(0, max_off) as u32,
            z_mm: oz.clamp(0, max_off) as u32,
            y_mm: mm(pos[1]) as i32,
            yaw_cdeg: (yaw_deg * 100.0).round() as i32,
            half_mm: half_m.map(|h| (h * 1000.0).round().max(1.0) as u32),
        }
    }
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
            Placement::Free(f) => vec![f.cell],
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
            // Свободные модели не занимают мест: их можно ставить сколько угодно рядом.
            (_, Placement::Free(_)) => Vec::new(),
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
            ) | (StructureKind::Freeform, Placement::Free(_))
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
        if let Placement::Free(f) = self.placement {
            let step_mm = (grid_step_m * 1000.0).round() as u32;
            if f.x_mm >= step_mm || f.z_mm >= step_mm {
                return Err(WorldError::InvalidPlacement(format!(
                    "смещение ({}, {}) мм выходит за якорную ячейку шагом {step_mm} мм",
                    f.x_mm, f.z_mm
                )));
            }
            if f.half_mm.iter().any(|&h| h == 0 || h > MAX_FREE_HALF_MM) {
                return Err(WorldError::InvalidPlacement(format!(
                    "полуразмеры {:?} мм: ожидается 1..={MAX_FREE_HALF_MM}",
                    f.half_mm
                )));
            }
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
