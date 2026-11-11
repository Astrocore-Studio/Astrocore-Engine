use std::fs;
use std::io::Write;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::GridEdge;
use astrocore_world::{
    FloorDef, FloorId, Placement, StructureElement, StructureKind, World, WorldCommand, WorldConfig,
};

use super::*;

fn new_world() -> World {
    let mut w = World::new("persist", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .unwrap();
    w
}

fn wall_cmd(id: u128, x: i32) -> WorldCommand {
    WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(id),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_x(x, 0)),
        floor: FloorId(0),
        material: PrototypeId::new("t:m").unwrap(),
        thickness: 0.2,
    })
}

#[test]
fn no_saves_means_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Store::open(dir.path())
            .unwrap()
            .recover()
            .unwrap()
            .is_none()
    );
}

#[test]
fn snapshot_roundtrip_keeps_ids_and_hash() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    w.apply(wall_cmd(7, 0)).unwrap();
    store.save_snapshot(&w).unwrap();
    let r = store.recover().unwrap().unwrap();
    assert_eq!(r.world.content_hash(), w.content_hash());
    assert!(r.world.element(ObjectId::from_u128(7)).is_some());
    assert_eq!((r.replayed, r.discarded_torn_tail), (0, false));
}

#[test]
fn journal_replays_on_top_of_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    store.save_snapshot(&w).unwrap();
    for i in 0..3 {
        let op = w.apply(wall_cmd(10 + i as u128, i)).unwrap();
        store.append(&op).unwrap();
    }
    let op = w
        .apply(WorldCommand::RemoveElement {
            id: ObjectId::from_u128(11),
        })
        .unwrap();
    store.append(&op).unwrap();

    let r = Store::open(dir.path()).unwrap().recover().unwrap().unwrap();
    assert_eq!(r.replayed, 4);
    assert_eq!(r.world.content_hash(), w.content_hash());
    assert!(r.world.element(ObjectId::from_u128(11)).is_none());
}

#[test]
fn replay_is_idempotent_when_snapshot_already_contains_ops() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    store.save_snapshot(&w).unwrap();
    let op = w.apply(wall_cmd(1, 0)).unwrap();
    store.append(&op).unwrap();
    // Сбой между записью нового снимка и очисткой журнала: имитируем, сохранив
    // снимок и вернув старый журнал на место.
    let journal = fs::read(dir.path().join("journal.jsonl")).unwrap();
    store.save_snapshot(&w).unwrap();
    fs::write(dir.path().join("journal.jsonl"), journal).unwrap();
    let r = store.recover().unwrap().unwrap();
    assert_eq!(
        r.replayed, 0,
        "уже учтённая операция не применяется второй раз"
    );
    assert_eq!(r.world.content_hash(), w.content_hash());
}

#[test]
fn torn_last_line_is_discarded_but_middle_corruption_is_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    store.save_snapshot(&w).unwrap();
    let op1 = w.apply(wall_cmd(1, 0)).unwrap();
    store.append(&op1).unwrap();
    // Оборванная запись (сбой питания посреди строки).
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("journal.jsonl"))
        .unwrap();
    write!(f, "deadbeef {{\"seq\":3,\"comm").unwrap();
    drop(f);
    let r = store.recover().unwrap().unwrap();
    assert!(r.discarded_torn_tail);
    assert_eq!(r.replayed, 1);

    // Теперь за оборванной строкой идёт нормальная — это уже повреждение в середине.
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("journal.jsonl"))
        .unwrap();
    writeln!(f).unwrap();
    let op2 = w.apply(wall_cmd(2, 1)).unwrap();
    let json = serde_json::to_string(&op2).unwrap();
    writeln!(f, "{:08x} {json}", crc32fast::hash(json.as_bytes())).unwrap();
    drop(f);
    assert!(matches!(
        store.recover(),
        Err(PersistError::CorruptJournal { .. })
    ));
}

#[test]
fn flipped_byte_in_journal_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    store.save_snapshot(&w).unwrap();
    for i in 0..2 {
        let op = w.apply(wall_cmd(i as u128, i)).unwrap();
        store.append(&op).unwrap();
    }
    let path = dir.path().join("journal.jsonl");
    let text =
        fs::read_to_string(&path)
            .unwrap()
            .replacen("\"thickness\":0.2", "\"thickness\":0.9", 1);
    fs::write(&path, text).unwrap();
    assert!(matches!(
        store.recover(),
        Err(PersistError::CorruptJournal { line: 1, .. })
    ));
}

#[test]
fn journal_gap_and_conflict_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    store.save_snapshot(&w).unwrap();
    let _lost = w.apply(wall_cmd(1, 0)).unwrap(); // не записана в журнал
    let op2 = w.apply(wall_cmd(2, 1)).unwrap();
    store.append(&op2).unwrap();
    assert!(matches!(
        store.recover(),
        Err(PersistError::JournalGap {
            expected: 2,
            got: 3
        })
    ));
}

#[test]
fn journal_without_snapshot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    let op = w.apply(wall_cmd(1, 0)).unwrap();
    store.append(&op).unwrap();
    assert!(matches!(
        store.recover(),
        Err(PersistError::JournalWithoutSnapshot)
    ));
}

#[test]
fn tampered_snapshot_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = new_world();
    w.apply(wall_cmd(1, 0)).unwrap();
    store.save_snapshot(&w).unwrap();
    let path = dir.path().join("snapshot.json");
    let original = fs::read_to_string(&path).unwrap();

    // Подменили содержимое, хеш остался старым.
    fs::write(
        &path,
        original.replace("\"thickness\": 0.2", "\"thickness\": 0.3"),
    )
    .unwrap();
    assert!(matches!(
        store.recover(),
        Err(PersistError::HashMismatch { .. })
    ));

    // Подменили ревизию мира.
    fs::write(
        &path,
        original.replace("\"world_revision\": 2", "\"world_revision\": 9"),
    )
    .unwrap();
    assert!(matches!(
        store.recover(),
        Err(PersistError::RevisionMismatch { .. })
    ));

    // Мусор вместо JSON.
    fs::write(&path, b"{ not json").unwrap();
    assert!(matches!(store.recover(), Err(PersistError::Json(_))));

    // Более новый формат.
    fs::write(
        &path,
        original.replace("\"format_version\": 1", "\"format_version\": 5"),
    )
    .unwrap();
    assert!(matches!(store.recover(), Err(PersistError::Migration(_))));
}

#[test]
fn golden_v1_snapshot_still_loads() {
    // Зафиксированный файл формата v1: если эта проверка ломается, изменилась схема
    // без миграции (правило: схемы не меняются без миграции и ADR).
    const GOLDEN: &str = include_str!("../tests/golden_snapshot_v1.json");
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("snapshot.json"), GOLDEN).unwrap();
    let r = Store::open(dir.path()).unwrap().recover().unwrap().unwrap();
    assert_eq!(r.world.id(), "golden");
    assert_eq!(r.world.revision().0, 2);
    assert!(r.world.element(ObjectId::from_u128(0xA11CE)).is_some());
}

#[test]
#[ignore = "генератор golden-файла; запускать вручную при осознанном изменении формата"]
fn generate_golden() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut w = World::new("golden", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "ground".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .unwrap();
    w.apply(wall_cmd(0xA11CE, 0)).unwrap();
    store.save_snapshot(&w).unwrap();
    fs::copy(
        dir.path().join("snapshot.json"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden_snapshot_v1.json"),
    )
    .unwrap();
}

mod upgrade {
    use astrocore_world::ReplicaOutcome;

    use super::*;
    use crate::upgrade::{ConflictKind, ConflictPolicy, Resolution, upgrade_map};

    fn wall(id: u128, x: i32, thickness: f32) -> WorldCommand {
        match wall_cmd(id, x) {
            WorldCommand::PlaceElement(mut e) => {
                e.thickness = thickness;
                WorldCommand::PlaceElement(e)
            }
            _ => unreachable!(),
        }
    }

    fn elements_of(w: &World) -> Vec<StructureElement> {
        let mut v: Vec<_> = w.elements().cloned().collect();
        v.sort_by_key(|e| e.id);
        v
    }

    fn id(n: u128) -> ObjectId {
        ObjectId::from_u128(n)
    }

    /// Версия 1: стены 1, 2, 3 на x = 0, 1, 2.
    fn base() -> World {
        let mut w = new_world();
        for (n, x) in [(1, 0), (2, 1), (3, 2)] {
            w.apply(wall(n, x, 0.2)).unwrap();
        }
        w
    }

    /// Версия 2: стена 1 толще, стена 2 убрана, добавлены 4 (x = 5) и 5 (x = 7).
    fn author_v2() -> World {
        let mut w = new_world();
        w.apply(wall(1, 0, 0.3)).unwrap();
        w.apply(wall(3, 2, 0.2)).unwrap();
        w.apply(wall(4, 5, 0.2)).unwrap();
        w.apply(wall(5, 7, 0.2)).unwrap();
        w
    }

    /// Сервер: версия 1, игроки утолстили стену 2 и построили свою стену 9 на месте будущей 5.
    fn runtime() -> World {
        let mut w = base();
        w.apply(WorldCommand::RemoveElement { id: id(2) }).unwrap();
        w.apply(wall(2, 1, 0.5)).unwrap();
        w.apply(wall(9, 7, 0.2)).unwrap();
        w
    }

    #[test]
    fn author_changes_merge_and_player_work_survives() {
        let before = runtime();
        let mut w = before.clone();
        let r = upgrade_map(&base(), &author_v2(), &mut w, ConflictPolicy::KeepRuntime);

        assert_eq!(
            w.element(id(1)).unwrap().thickness,
            0.3,
            "правка автора перенесена"
        );
        assert_eq!(
            w.element(id(2)).unwrap().thickness,
            0.5,
            "работа игроков не потеряна"
        );
        assert!(w.element(id(3)).is_some());
        assert!(w.element(id(4)).is_some(), "новый объект автора добавлен");
        assert!(w.element(id(9)).is_some(), "постройка игроков на месте");
        assert!(w.element(id(5)).is_none(), "не поместился");

        assert_eq!((r.added, r.modified, r.removed), (1, 1, 0));
        let kinds: Vec<_> = r
            .conflicts
            .iter()
            .map(|c| (c.id, c.kind, c.resolution))
            .collect();
        assert!(kinds.contains(&(
            Some(id(2)),
            ConflictKind::AuthorRemovedRuntimeModified,
            Resolution::KeptRuntime
        )));
        assert!(kinds.contains(&(
            Some(id(5)),
            ConflictKind::PlacementBlocked,
            Resolution::Skipped
        )));
        assert_eq!(r.conflicts.len(), 2);
        assert!(!r.is_clean());
        assert!(r.summary().contains("конфликтов 2"));

        // Журнал обновления воспроизводит то же состояние на копии до обновления.
        let mut replay = before;
        for op in &r.ops {
            assert!(matches!(
                replay.apply_replicated(op),
                ReplicaOutcome::Applied
            ));
        }
        assert_eq!(replay.content_hash(), w.content_hash());
    }

    #[test]
    fn prefer_author_takes_the_author_version_and_still_reports() {
        let mut w = runtime();
        let r = upgrade_map(&base(), &author_v2(), &mut w, ConflictPolicy::PreferAuthor);
        assert!(w.element(id(2)).is_none(), "автор убрал стену 2");
        assert!(
            w.element(id(9)).is_some(),
            "игроки не теряют постройки, которых автор не касался"
        );
        assert!(
            r.conflicts
                .iter()
                .any(|c| c.id == Some(id(2)) && c.resolution == Resolution::TookAuthor)
        );
    }

    #[test]
    fn upgrade_without_player_changes_equals_the_new_map_and_is_idempotent() {
        let mut w = base();
        let r = upgrade_map(&base(), &author_v2(), &mut w, ConflictPolicy::KeepRuntime);
        assert!(r.is_clean(), "{:?}", r.conflicts);
        assert_eq!(elements_of(&w), elements_of(&author_v2()));
        let again = upgrade_map(&base(), &author_v2(), &mut w, ConflictPolicy::KeepRuntime);
        assert!(again.ops.is_empty() && again.is_clean(), "{again:?}");
    }

    #[test]
    fn floors_added_by_the_author_appear_and_non_empty_ones_stay() {
        let mut new = author_v2();
        new.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(1),
            name: "up".into(),
            base_y: 3.0,
            height: 3.0,
        }))
        .unwrap();
        let mut w = base();
        upgrade_map(&base(), &new, &mut w, ConflictPolicy::KeepRuntime);
        assert!(w.floor(FloorId(1)).is_some());
        // Автор убрал этаж 0, но на нём стены игроков.
        let mut empty = World::new("persist", WorldConfig::default()).unwrap();
        empty
            .apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(5),
                name: "x".into(),
                base_y: 0.0,
                height: 3.0,
            }))
            .unwrap();
        let mut w = base();
        w.apply(wall(9, 20, 0.2)).unwrap();
        let r = upgrade_map(&base(), &empty, &mut w, ConflictPolicy::KeepRuntime);
        assert!(w.floor(FloorId(0)).is_some());
        assert!(
            r.conflicts
                .iter()
                .any(|c| c.kind == ConflictKind::FloorNotEmpty)
        );
    }
}
