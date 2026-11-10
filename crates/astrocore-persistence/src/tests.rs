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
