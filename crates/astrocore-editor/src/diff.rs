use std::collections::BTreeSet;

use astrocore_core::ObjectId;
use astrocore_world::{FloorId, World};

/// Различия двух миров на уровне объектов и этажей (панель «Различия» в Studio).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldDiff {
    pub added: Vec<ObjectId>,
    pub removed: Vec<ObjectId>,
    pub modified: Vec<ObjectId>,
    pub floors_added: Vec<FloorId>,
    pub floors_removed: Vec<FloorId>,
    pub floors_changed: Vec<FloorId>,
}

impl WorldDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
            && self.floors_added.is_empty()
            && self.floors_removed.is_empty()
            && self.floors_changed.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "Без изменений".into();
        }
        format!(
            "+{} −{} ~{} объектов; этажей: +{} −{} ~{}",
            self.added.len(),
            self.removed.len(),
            self.modified.len(),
            self.floors_added.len(),
            self.floors_removed.len(),
            self.floors_changed.len()
        )
    }
}

/// Сравнить `old` и `new`; списки отсортированы по идентификаторам.
pub fn diff_worlds(old: &World, new: &World) -> WorldDiff {
    let ids_old: BTreeSet<ObjectId> = old.elements().map(|e| e.id).collect();
    let ids_new: BTreeSet<ObjectId> = new.elements().map(|e| e.id).collect();
    let mut d = WorldDiff {
        added: ids_new.difference(&ids_old).copied().collect(),
        removed: ids_old.difference(&ids_new).copied().collect(),
        ..WorldDiff::default()
    };
    d.modified = ids_old
        .intersection(&ids_new)
        .filter(|id| old.element(**id) != new.element(**id))
        .copied()
        .collect();
    let f_old: BTreeSet<FloorId> = old.floors().map(|f| f.id).collect();
    let f_new: BTreeSet<FloorId> = new.floors().map(|f| f.id).collect();
    d.floors_added = f_new.difference(&f_old).copied().collect();
    d.floors_removed = f_old.difference(&f_new).copied().collect();
    d.floors_changed = f_old
        .intersection(&f_new)
        .filter(|id| old.floor(**id) != new.floor(**id))
        .copied()
        .collect();
    d
}
