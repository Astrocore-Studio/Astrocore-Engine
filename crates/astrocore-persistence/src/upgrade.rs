use std::collections::BTreeSet;

use astrocore_core::ObjectId;
use astrocore_world::{FloorId, World, WorldCommand, WorldOp};

/// Как разрешать конфликт, когда автор карты и состояние сервера изменили один объект.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Сохранить то, что есть на сервере (изменения игроков не теряются).
    #[default]
    KeepRuntime,
    /// Взять версию автора карты.
    PreferAuthor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Объект изменён и автором, и игроками по-разному.
    BothModified,
    /// Автор изменил объект, который игроки убрали.
    RuntimeRemovedAuthorModified,
    /// Автор убрал объект, который игроки изменили.
    AuthorRemovedRuntimeModified,
    /// Новое или изменённое размещение автора не поместилось (место занято постройкой игроков).
    PlacementBlocked,
    /// Определение этажа изменено автором: онлайн не применяется.
    FloorChanged,
    /// Автор убрал этаж, на котором есть элементы.
    FloorNotEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    KeptRuntime,
    TookAuthor,
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapConflict {
    pub id: Option<ObjectId>,
    pub floor: Option<FloorId>,
    pub kind: ConflictKind,
    pub resolution: Resolution,
    pub detail: String,
}

/// Итог обновления карты: что применено, какие конфликты и какие операции нужно записать
/// в журнал и разослать клиентам.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpgradeReport {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    pub unchanged_by_author: usize,
    pub conflicts: Vec<MapConflict>,
    pub ops: Vec<WorldOp>,
}

impl UpgradeReport {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "добавлено {}, изменено {}, удалено {}, конфликтов {}",
            self.added,
            self.modified,
            self.removed,
            self.conflicts.len()
        )
    }
}

/// Трёхстороннее обновление: `base` — карта автора версии N, `new` — версия N+1, `runtime` —
/// рабочий мир сервера (база плюс изменения игроков). Изменения автора переносятся в
/// `runtime`; изменения игроков не теряются, спорное попадает в отчёт. `ObjectId` объектов
/// сохраняются. Операции применяются обычными командами мира, поэтому проходят ту же валидацию.
pub fn upgrade_map(
    base: &World,
    new: &World,
    runtime: &mut World,
    policy: ConflictPolicy,
) -> UpgradeReport {
    let mut report = UpgradeReport::default();
    let ids_base: BTreeSet<ObjectId> = base.elements().map(|e| e.id).collect();
    let ids_new: BTreeSet<ObjectId> = new.elements().map(|e| e.id).collect();

    // Этажи: добавление применяется, удаление — только пустого, изменение — в отчёт.
    let floors_base: BTreeSet<FloorId> = base.floors().map(|f| f.id).collect();
    let floors_new: BTreeSet<FloorId> = new.floors().map(|f| f.id).collect();
    for def in new.floors() {
        match (floors_base.contains(&def.id), runtime.floor(def.id)) {
            (false, None) => {
                apply(runtime, WorldCommand::DefineFloor(def.clone()), &mut report);
            }
            (true, Some(cur)) if base.floor(def.id) != Some(def) && cur != def => {
                conflict(
                    &mut report,
                    None,
                    Some(def.id),
                    ConflictKind::FloorChanged,
                    Resolution::KeptRuntime,
                    "определение этажа изменено автором; онлайн не применяется",
                );
            }
            _ => {}
        }
    }

    // Удалённое автором.
    for id in ids_base.difference(&ids_new) {
        let (Some(was), Some(now)) = (base.element(*id), runtime.element(*id)) else {
            continue; // уже убрано игроками — согласуется
        };
        if was == now {
            if apply(
                runtime,
                WorldCommand::RemoveElement { id: *id },
                &mut report,
            ) {
                report.removed += 1;
            }
        } else if policy == ConflictPolicy::PreferAuthor {
            if apply(
                runtime,
                WorldCommand::RemoveElement { id: *id },
                &mut report,
            ) {
                report.removed += 1;
            }
            conflict(
                &mut report,
                Some(*id),
                None,
                ConflictKind::AuthorRemovedRuntimeModified,
                Resolution::TookAuthor,
                "автор убрал объект, изменённый игроками",
            );
        } else {
            conflict(
                &mut report,
                Some(*id),
                None,
                ConflictKind::AuthorRemovedRuntimeModified,
                Resolution::KeptRuntime,
                "автор убрал объект, изменённый игроками; оставлен",
            );
        }
    }

    // Изменённое автором.
    for id in ids_base.intersection(&ids_new) {
        let (Some(was), Some(is)) = (base.element(*id), new.element(*id)) else {
            continue;
        };
        if was == is {
            report.unchanged_by_author += 1;
            continue;
        }
        match runtime.element(*id).cloned() {
            None => {
                let take = policy == ConflictPolicy::PreferAuthor
                    && apply(runtime, WorldCommand::PlaceElement(is.clone()), &mut report);
                if take {
                    report.modified += 1;
                }
                conflict(
                    &mut report,
                    Some(*id),
                    None,
                    ConflictKind::RuntimeRemovedAuthorModified,
                    if take {
                        Resolution::TookAuthor
                    } else {
                        Resolution::KeptRuntime
                    },
                    "автор изменил объект, убранный игроками",
                );
            }
            Some(now) if now == *was => replace(runtime, &now, is, &mut report),
            Some(now) if now == *is => {} // уже совпадает
            Some(now) => {
                if policy == ConflictPolicy::PreferAuthor {
                    replace(runtime, &now, is, &mut report);
                }
                conflict(
                    &mut report,
                    Some(*id),
                    None,
                    ConflictKind::BothModified,
                    if policy == ConflictPolicy::PreferAuthor {
                        Resolution::TookAuthor
                    } else {
                        Resolution::KeptRuntime
                    },
                    "объект изменён и автором, и игроками",
                );
            }
        }
    }

    // Добавленное автором.
    for id in ids_new.difference(&ids_base) {
        let Some(el) = new.element(*id) else { continue };
        if runtime.element(*id).is_some() {
            continue; // уже есть (повторное применение обновления)
        }
        match runtime.apply(WorldCommand::PlaceElement(el.clone())) {
            Ok(op) => {
                report.added += 1;
                report.ops.push(op);
            }
            Err(e) => conflict(
                &mut report,
                Some(*id),
                Some(el.floor),
                ConflictKind::PlacementBlocked,
                Resolution::Skipped,
                &format!("новый объект не помещается: {e}"),
            ),
        }
    }

    // Удалённые автором этажи — после элементов.
    for id in floors_base.difference(&floors_new) {
        if runtime.floor(*id).is_none() {
            continue;
        }
        match runtime.apply(WorldCommand::RemoveFloor { id: *id }) {
            Ok(op) => report.ops.push(op),
            Err(e) => conflict(
                &mut report,
                None,
                Some(*id),
                ConflictKind::FloorNotEmpty,
                Resolution::KeptRuntime,
                &format!("этаж оставлен: {e}"),
            ),
        }
    }
    report
}

fn apply(runtime: &mut World, command: WorldCommand, report: &mut UpgradeReport) -> bool {
    match runtime.apply(command) {
        Ok(op) => {
            report.ops.push(op);
            true
        }
        Err(_) => false,
    }
}

/// Заменить объект версией автора; при отказе вернуть прежний и записать конфликт.
fn replace(
    runtime: &mut World,
    current: &astrocore_world::StructureElement,
    author: &astrocore_world::StructureElement,
    report: &mut UpgradeReport,
) {
    let id = current.id;
    let Ok(remove) = runtime.apply(WorldCommand::RemoveElement { id }) else {
        return;
    };
    match runtime.apply(WorldCommand::PlaceElement(author.clone())) {
        Ok(place) => {
            report.ops.push(remove);
            report.ops.push(place);
            report.modified += 1;
        }
        Err(e) => {
            // Откат: прежний объект возвращается; операции удаления и возврата попадают в журнал.
            if let Ok(back) = runtime.apply(WorldCommand::PlaceElement(current.clone())) {
                report.ops.push(remove);
                report.ops.push(back);
            }
            conflict(
                report,
                Some(id),
                Some(author.floor),
                ConflictKind::PlacementBlocked,
                Resolution::KeptRuntime,
                &format!("версия автора не помещается: {e}"),
            );
        }
    }
}

fn conflict(
    report: &mut UpgradeReport,
    id: Option<ObjectId>,
    floor: Option<FloorId>,
    kind: ConflictKind,
    resolution: Resolution,
    detail: &str,
) {
    report.conflicts.push(MapConflict {
        id,
        floor,
        kind,
        resolution,
        detail: detail.into(),
    });
}
