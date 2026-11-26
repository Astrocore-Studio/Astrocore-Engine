//! Прототипы Astrocore: данные игры в YAML (формат SS14) и RON (собственный формат),
//! наследование как в Robust и создание сущностей из прототипов.

mod inherit;
mod manager;
mod spawn;

pub use inherit::{FieldRules, MAX_INHERITANCE_DEPTH, merge_maps, merge_nodes};
pub use manager::{
    ENTITY_KIND, EntProtoId, EntityPrototype, LoadReport, ProtoId, Prototype, PrototypeComponent,
    PrototypeError, Prototypes,
};
pub use spawn::{EntityOverrides, PrototypeWorldExt, SpawnError};

#[cfg(test)]
mod tests;
