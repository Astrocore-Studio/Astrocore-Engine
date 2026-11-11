use std::collections::BTreeMap;

use astrocore_core::{PrefabInstanceId, PrefabNodeId, PrototypeId};
use astrocore_math::{Facing, GridCell, GridEdge, GridTransform};
use astrocore_world::{OpeningKind, Placement, StructureKind};
use serde::{Deserialize, Serialize};

/// Путь вложенного узла: цепочка `PrefabNodeId` от корня экземпляра.
pub type NodePath = Vec<PrefabNodeId>;

/// Размещение в локальных координатах префаба.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalPlacement {
    Edge(GridEdge),
    Cell(GridCell),
    Run {
        start: GridCell,
        dir: Facing,
        length: u8,
    },
}

impl LocalPlacement {
    /// Преобразование без выхода из локальных координат (для цепочек вложенности).
    pub fn apply(self, t: GridTransform) -> LocalPlacement {
        match self {
            LocalPlacement::Edge(e) => LocalPlacement::Edge(t.edge(e)),
            LocalPlacement::Cell(c) => LocalPlacement::Cell(t.cell(c)),
            LocalPlacement::Run { start, dir, length } => {
                let (start, dir) = t.run(start, dir);
                LocalPlacement::Run { start, dir, length }
            }
        }
    }

    pub fn into_world(self) -> Placement {
        match self {
            LocalPlacement::Edge(e) => Placement::Edge(e),
            LocalPlacement::Cell(c) => Placement::Cell(c),
            LocalPlacement::Run { start, dir, length } => Placement::Run { start, dir, length },
        }
    }

    pub fn transform(self, t: GridTransform) -> Placement {
        match self {
            LocalPlacement::Edge(e) => Placement::Edge(t.edge(e)),
            LocalPlacement::Cell(c) => Placement::Cell(t.cell(c)),
            LocalPlacement::Run { start, dir, length } => {
                let (start, dir) = t.run(start, dir);
                Placement::Run { start, dir, length }
            }
        }
    }
}

/// Вид элемента с этажами **относительно** базового этажа экземпляра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalKind {
    Wall,
    Opening(OpeningKind),
    Floor,
    Ceiling,
    Stair { to: i32 },
    Ramp { to: i32 },
}

impl LocalKind {
    pub fn to_world(self, floor_shift: i32) -> StructureKind {
        use astrocore_world::FloorId;
        match self {
            LocalKind::Wall => StructureKind::Wall,
            LocalKind::Opening(k) => StructureKind::Opening(k),
            LocalKind::Floor => StructureKind::Floor,
            LocalKind::Ceiling => StructureKind::Ceiling,
            LocalKind::Stair { to } => StructureKind::Stair {
                to: FloorId(to + floor_shift),
            },
            LocalKind::Ramp { to } => StructureKind::Ramp {
                to: FloorId(to + floor_shift),
            },
        }
    }
}

/// Шаблон структурного элемента.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementTemplate {
    pub kind: LocalKind,
    pub placement: LocalPlacement,
    /// Этаж относительно базового этажа экземпляра.
    pub floor_offset: i32,
    pub material: PrototypeId,
    pub thickness: f32,
}

/// Вложенный экземпляр другого префаба.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildRef {
    pub prefab: PrototypeId,
    /// Требуемая версия вложенного префаба; несовпадение — ошибка проверки.
    pub required_version: Option<u32>,
    pub transform: GridTransform,
    pub floor_offset: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeContent {
    Element(ElementTemplate),
    Child(ChildRef),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabNode {
    pub id: PrefabNodeId,
    pub name: String,
    pub content: NodeContent,
}

/// Переопределение узла. Пустые поля означают «как в источнике».
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeOverride {
    pub material: Option<PrototypeId>,
    pub thickness: Option<f32>,
    /// Убрать узел из результата.
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathOverride {
    pub path: NodePath,
    pub value: NodeOverride,
}

/// Определение префаба.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabDef {
    pub id: PrototypeId,
    /// Версия определения; растёт при несовместимых изменениях.
    pub version: u32,
    /// Родитель для наследования (вариант = потомок + переопределения).
    pub parent: Option<PrototypeId>,
    /// Узлы: одноимённые по `PrefabNodeId` узлы родителя заменяются, остальные добавляются.
    pub nodes: Vec<PrefabNode>,
    /// Переопределения унаследованных узлов (по `PrefabNodeId` верхнего уровня родителя).
    pub overrides: BTreeMap<PrefabNodeId, NodeOverride>,
}

impl PrefabDef {
    pub fn new(id: PrototypeId) -> Self {
        Self {
            id,
            version: 1,
            parent: None,
            nodes: Vec::new(),
            overrides: BTreeMap::new(),
        }
    }
}

/// Экземпляр префаба в мире.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabInstance {
    pub id: PrefabInstanceId,
    pub prefab: PrototypeId,
    pub transform: GridTransform,
    pub base_floor: i32,
    /// Явные переопределения экземпляра; сохраняются при обновлении префаба.
    pub overrides: Vec<PathOverride>,
}

impl PrefabInstance {
    pub fn new(id: PrefabInstanceId, prefab: PrototypeId) -> Self {
        Self {
            id,
            prefab,
            transform: GridTransform::IDENTITY,
            base_floor: 0,
            overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PrefabError {
    #[error("префаб {0} не найден")]
    Missing(PrototypeId),
    #[error("циклическая зависимость префабов: {0}")]
    Cycle(String),
    #[error("слишком глубокая вложенность префабов (> {0})")]
    TooDeep(usize),
    #[error("префаб {prefab}: требуется версия {required} префаба {child}, есть {actual}")]
    VersionMismatch {
        prefab: PrototypeId,
        child: PrototypeId,
        required: u32,
        actual: u32,
    },
    #[error("префаб {prefab}: узел {node:?} повторяется")]
    DuplicateNode {
        prefab: PrototypeId,
        node: PrefabNodeId,
    },
    #[error("префаб {0} уже зарегистрирован")]
    DuplicatePrefab(PrototypeId),
    #[error("два узла занимают одно место: {0}")]
    SlotCollision(String),
    #[error("неверный размер: {0}")]
    InvalidDimension(String),
    #[error("файл библиотеки: {0}")]
    File(String),
}
