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
