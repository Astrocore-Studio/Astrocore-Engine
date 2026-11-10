use astrocore_core::{ObjectId, PrototypeId, Revision};
use astrocore_math::{ChunkCoord, GridCell, GridEdge};

use super::*;

fn mat() -> PrototypeId {
    PrototypeId::new("test:stone").unwrap()
}

fn world() -> World {
    let mut w = World::new("t", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "ground".into(),
        base_y: 0.0,
        height: 4.0,
    }))
    .unwrap();
    w
}

fn wall(id: u128, edge: GridEdge) -> StructureElement {
    StructureElement {
        id: ObjectId::from_u128(id),
        kind: StructureKind::Wall,
        placement: Placement::Edge(edge),
        floor: FloorId(0),
        material: mat(),
        thickness: 0.2,
    }
}

#[test]
fn revision_increases_with_each_applied_command() {
    let mut w = world();
    assert_eq!(w.revision(), Revision(1));
    let op = w
        .apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    assert_eq!(op.seq, 2);
    assert_eq!(w.revision(), Revision(2));
}

#[test]
fn rejected_command_does_not_change_world() {
    let mut w = world();
    w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    let before = w.content_hash();
    let rev = w.revision();

    // Занятое место, дубль ID, неизвестный этаж, неверные размеры, неверное размещение.
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(wall(2, GridEdge::along_x(0, 0)))),
        Err(WorldError::SlotOccupied { .. })
    ));
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(5, 5)))),
        Err(WorldError::DuplicateId(_))
    ));
    let mut bad_floor = wall(3, GridEdge::along_x(9, 9));
    bad_floor.floor = FloorId(7);
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(bad_floor)),
        Err(WorldError::UnknownFloor(_))
    ));
    let mut bad_dim = wall(4, GridEdge::along_x(9, 9));
    bad_dim.thickness = f32::NAN;
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(bad_dim)),
        Err(WorldError::InvalidDimension(_))
    ));
    let mut bad_place = wall(5, GridEdge::along_x(9, 9));
    bad_place.placement = Placement::Cell(GridCell::new(0, 0));
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(bad_place)),
        Err(WorldError::InvalidPlacement(_))
    ));
    assert!(matches!(
        w.apply(WorldCommand::RemoveElement {
            id: ObjectId::from_u128(99)
        }),
        Err(WorldError::UnknownElement(_))
    ));

    assert_eq!(w.content_hash(), before);
    assert_eq!(w.revision(), rev);
    assert!(
        !w.take_dirty().is_empty(),
        "dirty от успешной первой стены сохраняется"
    );
}

#[test]
fn stale_revision_is_rejected() {
    let mut w = world();
    let seen = w.revision();
    w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    let r = w.apply_checked(
        WorldCommand::PlaceElement(wall(2, GridEdge::along_x(1, 0))),
        seen,
    );
    assert!(matches!(r, Err(WorldError::StaleRevision { .. })));
    let ok = w.apply_checked(
        WorldCommand::PlaceElement(wall(2, GridEdge::along_x(1, 0))),
        w.revision(),
    );
    assert!(ok.is_ok());
}

#[test]
fn placing_and_removing_marks_dirty_chunks_and_revisions() {
    let mut w = world();
    w.take_dirty();
    // Ребро на границе чанков 0 и 1: ячейки (31, 0) и (32, 0).
    w.apply(WorldCommand::PlaceElement(wall(
        1,
        GridEdge::along_z(32, 0),
    )))
    .unwrap();
    let dirty = w.take_dirty();
    let chunks: Vec<_> = dirty.iter().map(|d| d.chunk).collect();
    assert_eq!(chunks, vec![ChunkCoord::new(0, 0), ChunkCoord::new(1, 0)]);
    assert!(dirty.iter().all(|d| d.flags.contains(DirtyFlags::TOPOLOGY)));
    assert_eq!(w.chunk_revision(ChunkCoord::new(1, 0)), w.revision());
    assert!(!w.has_dirty());

    w.apply(WorldCommand::RemoveElement {
        id: ObjectId::from_u128(1),
    })
    .unwrap();
    assert_eq!(w.take_dirty().len(), 2);
}

#[test]
fn negative_coordinates_land_in_negative_chunks() {
    let mut w = world();
    w.take_dirty();
    w.apply(WorldCommand::PlaceElement(wall(
        1,
        GridEdge::along_x(-5, -5),
    )))
    .unwrap();
    let chunks: Vec<_> = w.take_dirty().into_iter().map(|d| d.chunk).collect();
    assert_eq!(chunks, vec![ChunkCoord::new(-1, -1)]);
    assert_eq!(
        w.elements_in_chunk(ChunkCoord::new(-1, -1), FloorId(0))
            .count(),
        1
    );
}

#[test]
fn indexes_follow_removal() {
    let mut w = world();
    let e = wall(1, GridEdge::along_x(2, 2));
    w.apply(WorldCommand::PlaceElement(e.clone())).unwrap();
    assert!(
        w.edge_element(FloorId(0), GridEdge::along_x(2, 2))
            .is_some()
    );
    w.apply(WorldCommand::RemoveElement { id: e.id }).unwrap();
    assert!(
        w.edge_element(FloorId(0), GridEdge::along_x(2, 2))
            .is_none()
    );
    assert_eq!(w.populated_chunks(FloorId(0)), Vec::<ChunkCoord>::new());
    // Освобождённое место можно занять снова.
    w.apply(WorldCommand::PlaceElement(wall(2, GridEdge::along_x(2, 2))))
        .unwrap();
}

#[test]
fn floor_bounds_cover_all_touched_cells() {
    let mut w = world();
    assert_eq!(w.floor_cell_bounds(FloorId(0)), None);
    w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    w.apply(WorldCommand::PlaceElement(wall(2, GridEdge::along_z(4, 3))))
        .unwrap();
    let (lo, hi) = w.floor_cell_bounds(FloorId(0)).unwrap();
    assert_eq!((lo, hi), (GridCell::new(0, -1), GridCell::new(4, 3)));
}

#[test]
fn state_roundtrip_preserves_content_and_ids() {
    let mut w = world();
    for i in 0..5 {
        w.apply(WorldCommand::PlaceElement(wall(
            10 + i as u128,
            GridEdge::along_x(i, 0),
        )))
        .unwrap();
    }
    let json = serde_json::to_string(&w.to_state()).unwrap();
    let restored = World::from_state(serde_json::from_str(&json).unwrap()).unwrap();
    assert_eq!(restored.content_hash(), w.content_hash());
    assert_eq!(restored.revision(), w.revision());
    assert!(restored.element(ObjectId::from_u128(12)).is_some());
    assert!(
        restored
            .edge_element(FloorId(0), GridEdge::along_x(3, 0))
            .is_some()
    );
}

#[test]
fn from_state_rejects_corrupt_data() {
    let mut w = world();
    w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();

    let mut st = w.to_state();
    st.schema_version = astrocore_core::SchemaVersion(99);
    assert!(matches!(
        World::from_state(st),
        Err(WorldError::UnsupportedSchema { .. })
    ));

    let mut st = w.to_state();
    st.elements.push(wall(2, GridEdge::along_x(0, 0))); // тот же слот
    assert!(matches!(
        World::from_state(st),
        Err(WorldError::SlotOccupied { .. })
    ));

    let mut st = w.to_state();
    st.elements.push(st.elements[0].clone()); // тот же ID
    assert!(World::from_state(st).is_err());

    let mut st = w.to_state();
    st.elements[0].floor = FloorId(42);
    assert!(matches!(
        World::from_state(st),
        Err(WorldError::UnknownFloor(_))
    ));

    let mut st = w.to_state();
    st.chunk_revisions
        .push((ChunkCoord::new(0, 0), Revision(1000)));
    assert!(matches!(
        World::from_state(st),
        Err(WorldError::CorruptState(_))
    ));

    let mut st = w.to_state();
    st.config.grid_step_m = 0.0;
    assert!(matches!(
        World::from_state(st),
        Err(WorldError::InvalidConfig(_))
    ));
}

#[test]
fn config_requires_chunk_multiple_of_grid() {
    let bad = WorldConfig {
        chunk_size_m: 10.0,
        grid_step_m: 3.0,
    };
    assert!(World::new("x", bad).is_err());
    let ok = WorldConfig {
        chunk_size_m: 12.0,
        grid_step_m: 3.0,
    };
    assert!(World::new("x", ok).is_ok());
}

#[test]
fn replication_is_ordered_and_idempotent() {
    let mut server = world();
    let mut client = World::new("t", WorldConfig::default()).unwrap();
    let mut log = vec![WorldOp {
        seq: 1,
        command: WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "ground".into(),
            base_y: 0.0,
            height: 4.0,
        }),
    }];
    log.push(
        server
            .apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
            .unwrap(),
    );
    log.push(
        server
            .apply(WorldCommand::PlaceElement(wall(2, GridEdge::along_x(1, 0))))
            .unwrap(),
    );

    // Пропуск операции → Gap, состояние не меняется.
    assert_eq!(
        client.apply_replicated(&log[1]),
        ReplicaOutcome::Gap {
            expected: 1,
            got: 2
        }
    );
    assert_eq!(client.revision(), Revision::ZERO);

    assert_eq!(client.apply_replicated(&log[0]), ReplicaOutcome::Applied);
    assert_eq!(client.apply_replicated(&log[1]), ReplicaOutcome::Applied);
    // Повтор подтверждённой операции не применяется второй раз.
    assert_eq!(client.apply_replicated(&log[1]), ReplicaOutcome::Duplicate);
    assert_eq!(client.apply_replicated(&log[2]), ReplicaOutcome::Applied);
    assert_eq!(client.content_hash(), server.content_hash());
}

#[test]
fn replicated_conflicting_op_is_rejected_without_change() {
    let mut client = world();
    client
        .apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    let before = client.content_hash();
    // Операция, которая следующая по номеру, но противоречит состоянию клиента.
    let op = WorldOp {
        seq: 3,
        command: WorldCommand::PlaceElement(wall(2, GridEdge::along_x(0, 0))),
    };
    assert!(matches!(
        client.apply_replicated(&op),
        ReplicaOutcome::Rejected(WorldError::SlotOccupied { .. })
    ));
    assert_eq!(client.content_hash(), before);
    assert_eq!(client.revision(), Revision(2));
}

#[test]
fn floor_and_ceiling_slabs_do_not_collide_with_walls() {
    let mut w = world();
    let cell = GridCell::new(0, 0);
    let slab = |id: u128, kind| StructureElement {
        id: ObjectId::from_u128(id),
        kind,
        placement: Placement::Cell(cell),
        floor: FloorId(0),
        material: mat(),
        thickness: 0.3,
    };
    w.apply(WorldCommand::PlaceElement(slab(1, StructureKind::Floor)))
        .unwrap();
    w.apply(WorldCommand::PlaceElement(slab(2, StructureKind::Ceiling)))
        .unwrap();
    w.apply(WorldCommand::PlaceElement(wall(3, GridEdge::along_x(0, 0))))
        .unwrap();
    assert!(matches!(
        w.apply(WorldCommand::PlaceElement(slab(4, StructureKind::Floor))),
        Err(WorldError::SlotOccupied { .. })
    ));
}

#[test]
fn invalid_floor_is_rejected() {
    let mut w = World::new("t", WorldConfig::default()).unwrap();
    let r = w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "bad".into(),
        base_y: 0.0,
        height: 0.0,
    }));
    assert!(matches!(r, Err(WorldError::InvalidFloor(_))));
    assert_eq!(w.revision(), Revision::ZERO);
}
