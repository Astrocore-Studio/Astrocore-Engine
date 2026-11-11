use astrocore_core::{IdError, ObjectId, Revision};

use crate::{FloorId, Slot};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WorldError {
    #[error("неверные настройки мира: {0}")]
    InvalidConfig(String),
    #[error("неизвестный этаж {0:?}")]
    UnknownFloor(FloorId),
    #[error("этаж {0:?} уже определён")]
    DuplicateFloor(FloorId),
    #[error("этаж {0:?} не пуст: на нём есть элементы или ведущие на него лестницы")]
    FloorNotEmpty(FloorId),
    #[error("неверное определение этажа: {0}")]
    InvalidFloor(String),
    #[error("объект {0} уже существует")]
    DuplicateId(ObjectId),
    #[error("объект {0} не найден")]
    UnknownElement(ObjectId),
    #[error("место {slot:?} уже занято объектом {by}")]
    SlotOccupied { slot: Slot, by: ObjectId },
    #[error("недопустимое размещение: {0}")]
    InvalidPlacement(String),
    #[error("недопустимый размер элемента: {0}")]
    InvalidDimension(String),
    #[error("устаревшая ревизия: ожидалась {expected:?}, фактическая {actual:?}")]
    StaleRevision {
        expected: Revision,
        actual: Revision,
    },
    #[error("версия схемы {found} не поддерживается (текущая {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("недопустимый авторский объект: {0}")]
    InvalidAuthored(String),
    #[error("повреждённое состояние мира: {0}")]
    CorruptState(String),
    #[error(transparent)]
    Id(#[from] IdError),
}
