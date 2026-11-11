//! Префабы (раздел 16 дизайн-документа).
//!
//! Возможности: вложенные экземпляры, наследование (варианты), переопределения на
//! уровне префаба и экземпляра, версионированные ссылки, обнаружение циклов,
//! стабильные идентификаторы узлов, отделение экземпляра от префаба, обновление
//! экземпляров мира после изменения префаба с отчётом о конфликтах.
//!
//! Идентичность. Каждый узел префаба имеет стабильный [`PrefabNodeId`]. Объект,
//! порождённый узлом, получает `ObjectId`, **выведенный** из `PrefabInstanceId` и
//! цепочки узлов (UUID v5): тот же узел того же экземпляра при любом обновлении и
//! перезапуске получает тот же `ObjectId`. Идентификаторы не зависят от координат,
//! имён и порядка узлов.

mod model;
mod registry;
mod resolve;
mod update;

pub use model::*;
pub use registry::{
    MAX_PREFAB_DEPTH, NodeChange, PREFAB_SCHEMA_VERSION, PrefabLibraryFile, PrefabRegistry,
    diff_defs,
};
pub use resolve::{ResolvedNode, materialize, object_id_for, resolve};
pub use update::{Conflict, ConflictKind, Reconciliation, detach, reconcile};

#[cfg(test)]
mod tests;
