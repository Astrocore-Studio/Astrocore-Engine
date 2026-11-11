use std::collections::BTreeSet;

use astrocore_core::ObjectId;
use astrocore_world::{FloorId, World, WorldCommand};

use crate::model::*;
use crate::registry::PrefabRegistry;
use crate::resolve::materialize;

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictKind {
    /// Место занято объектом, не принадлежащим экземпляру.
    SlotOccupied {
        by: ObjectId,
    },
    FloorMissing(FloorId),
    Invalid(String),
}

/// Элемент, который не удалось создать при обновлении. Мир в этом месте не тронут.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub object: ObjectId,
    pub kind: ConflictKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reconciliation {
    /// Команды в порядке применения: сначала удаления, затем создания.
    pub commands: Vec<WorldCommand>,
    /// Новый набор объектов экземпляра (то, что останется в мире после применения).
    pub spawned: BTreeSet<ObjectId>,
    pub conflicts: Vec<Conflict>,
}

/// Привести мир в соответствие с экземпляром после изменения префаба (или впервые
/// создать экземпляр: `spawned` пуст).
///
/// * объекты, которых больше нет в префабе, удаляются;
/// * изменившиеся — пересоздаются с тем же `ObjectId`;
/// * неизменные не трогаются;
/// * если место занято чужим объектом, создание пропускается и сообщается как
///   конфликт — чужие объекты не уничтожаются молча.
///
/// Переопределения экземпляра живут в `PrefabInstance.overrides` и участвуют в
/// `materialize`, поэтому обновление префаба их не затирает.
pub fn reconcile(
    reg: &PrefabRegistry,
    instance: &PrefabInstance,
    spawned: &BTreeSet<ObjectId>,
    world: &World,
) -> Result<Reconciliation, PrefabError> {
    let desired = materialize(reg, instance)?;
    let mut result = Reconciliation::default();

    let mut removed: BTreeSet<ObjectId> = BTreeSet::new();
    for id in spawned {
        if let Some(current) = world.element(*id)
            && desired.get(id) != Some(current)
        {
            removed.insert(*id);
        }
    }
    for id in &removed {
        result
            .commands
            .push(WorldCommand::RemoveElement { id: *id });
    }

    let step = world.config().grid_step_m;
    let mut claimed = BTreeSet::new();
    for (id, element) in &desired {
        let present = world.element(*id).is_some() && !removed.contains(id);
        if present {
            result.spawned.insert(*id);
            continue;
        }
        let floors_ok = [Some(element.floor), element.kind.target_floor()]
            .into_iter()
            .flatten()
            .find(|f| world.floor(*f).is_none());
        if let Some(missing) = floors_ok {
            result.conflicts.push(Conflict {
                object: *id,
                kind: ConflictKind::FloorMissing(missing),
            });
            continue;
        }
        if let Err(e) = element.validate(step) {
            result.conflicts.push(Conflict {
                object: *id,
                kind: ConflictKind::Invalid(e.to_string()),
            });
            continue;
        }
        let occupied_by = element.slots().into_iter().find_map(|slot| {
            world
                .element_at(slot.floor, slot.key)
                .map(|e| e.id)
                .filter(|other| !removed.contains(other) && other != id)
        });
        if let Some(by) = occupied_by {
            result.conflicts.push(Conflict {
                object: *id,
                kind: ConflictKind::SlotOccupied { by },
            });
            continue;
        }
        // Два желаемых элемента не пересекаются (проверено в materialize).
        claimed.insert(*id);
        result
            .commands
            .push(WorldCommand::PlaceElement(element.clone()));
        result.spawned.insert(*id);
    }
    debug_assert!(claimed.iter().all(|id| result.spawned.contains(id)));
    Ok(result)
}

/// Отделить объекты экземпляра от префаба: каждый получает собственный `ObjectId`
/// (выдаётся `mint`) и больше не обновляется вместе с префабом.
/// Возвращает команды (сначала удаления, затем создания) и соответствие старых ID новым.
pub fn detach(
    world: &World,
    spawned: &BTreeSet<ObjectId>,
    mut mint: impl FnMut() -> ObjectId,
) -> (Vec<WorldCommand>, Vec<(ObjectId, ObjectId)>) {
    let mut removes = Vec::new();
    let mut places = Vec::new();
    let mut mapping = Vec::new();
    for old in spawned {
        let Some(element) = world.element(*old) else {
            continue;
        };
        let new = mint();
        removes.push(WorldCommand::RemoveElement { id: *old });
        let mut copy = element.clone();
        copy.id = new;
        places.push(WorldCommand::PlaceElement(copy));
        mapping.push((*old, new));
    }
    removes.extend(places);
    (removes, mapping)
}
