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

mod stairs {
    use astrocore_math::Facing;

    use super::*;

    fn two_floor_world() -> World {
        let mut w = world();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(1),
            name: "up".into(),
            base_y: 4.0,
            height: 4.0,
        }))
        .unwrap();
        w
    }

    fn stair(id: u128, start: GridCell, length: u8, to: FloorId) -> StructureElement {
        StructureElement {
            id: ObjectId::from_u128(id),
            kind: StructureKind::Stair { to },
            placement: Placement::Run {
                start,
                dir: Facing::East,
                length,
            },
            floor: FloorId(0),
            material: mat(),
            thickness: 0.2,
        }
    }

    #[test]
    fn stair_occupies_every_cell_of_its_run_and_creates_a_link() {
        let mut w = two_floor_world();
        w.apply(WorldCommand::PlaceElement(stair(
            1,
            GridCell::new(2, 2),
            4,
            FloorId(1),
        )))
        .unwrap();
        for x in 2..6 {
            assert!(
                w.element_at(FloorId(0), SlotKey::Run(GridCell::new(x, 2)))
                    .is_some(),
                "x={x}"
            );
        }
        assert!(
            w.element_at(FloorId(0), SlotKey::Run(GridCell::new(6, 2)))
                .is_none()
        );
        let links = w.vertical_links();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].lower, (FloorId(0), GridCell::new(2, 2)));
        assert_eq!(
            links[0].upper,
            (FloorId(1), GridCell::new(6, 2)),
            "выход на верхнем этаже за маршем"
        );
    }

    #[test]
    fn overlapping_runs_are_rejected_and_removal_frees_all_cells() {
        let mut w = two_floor_world();
        w.apply(WorldCommand::PlaceElement(stair(
            1,
            GridCell::new(2, 2),
            4,
            FloorId(1),
        )))
        .unwrap();
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(stair(
                2,
                GridCell::new(4, 2),
                3,
                FloorId(1)
            ))),
            Err(WorldError::SlotOccupied { .. })
        ));
        w.apply(WorldCommand::RemoveElement {
            id: ObjectId::from_u128(1),
        })
        .unwrap();
        w.apply(WorldCommand::PlaceElement(stair(
            2,
            GridCell::new(4, 2),
            3,
            FloorId(1),
        )))
        .unwrap();
        assert!(
            w.element_at(FloorId(0), SlotKey::Run(GridCell::new(2, 2)))
                .is_none()
        );
    }

    #[test]
    fn invalid_stairs_are_rejected() {
        let mut w = two_floor_world();
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(stair(
                1,
                GridCell::new(0, 0),
                0,
                FloorId(1)
            ))),
            Err(WorldError::InvalidPlacement(_))
        ));
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(stair(
                1,
                GridCell::new(0, 0),
                200,
                FloorId(1)
            ))),
            Err(WorldError::InvalidPlacement(_))
        ));
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(stair(
                1,
                GridCell::new(0, 0),
                3,
                FloorId(9)
            ))),
            Err(WorldError::UnknownFloor(_))
        ));
        // «Лестница вниз» (на этаж, который не выше) — ошибка.
        let mut down = stair(2, GridCell::new(0, 0), 3, FloorId(0));
        down.floor = FloorId(1);
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(down)),
            Err(WorldError::InvalidPlacement(_))
        ));
        // Лестница с размещением по ребру — ошибка.
        let mut bad = stair(3, GridCell::new(0, 0), 3, FloorId(1));
        bad.placement = Placement::Edge(GridEdge::along_x(0, 0));
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(bad)),
            Err(WorldError::InvalidPlacement(_))
        ));
        assert_eq!(w.element_count(), 0);
    }

    #[test]
    fn stairs_survive_state_roundtrip_and_replication() {
        let mut w = two_floor_world();
        w.apply(WorldCommand::PlaceElement(stair(
            1,
            GridCell::new(2, 2),
            4,
            FloorId(1),
        )))
        .unwrap();
        let restored = World::from_state(w.to_state()).unwrap();
        assert_eq!(restored.content_hash(), w.content_hash());
        assert_eq!(restored.vertical_links(), w.vertical_links());
    }

    #[test]
    fn run_touches_chunks_across_a_boundary() {
        let mut w = two_floor_world();
        w.take_dirty();
        w.apply(WorldCommand::PlaceElement(stair(
            1,
            GridCell::new(30, 2),
            5,
            FloorId(1),
        )))
        .unwrap();
        let chunks: Vec<_> = w.take_dirty().into_iter().map(|d| d.chunk).collect();
        assert_eq!(chunks, vec![ChunkCoord::new(0, 0), ChunkCoord::new(1, 0)]);
    }
}

#[test]
fn empty_floors_can_be_removed_and_nonempty_cannot() {
    let mut w = world();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(1),
        name: "up".into(),
        base_y: 4.0,
        height: 3.0,
    }))
    .unwrap();
    w.apply(WorldCommand::PlaceElement(wall(1, GridEdge::along_x(0, 0))))
        .unwrap();
    assert!(matches!(
        w.apply(WorldCommand::RemoveFloor { id: FloorId(0) }),
        Err(WorldError::FloorNotEmpty(_))
    ));
    assert!(matches!(
        w.apply(WorldCommand::RemoveFloor { id: FloorId(7) }),
        Err(WorldError::UnknownFloor(_))
    ));
    let before = w.revision();
    w.apply(WorldCommand::RemoveFloor { id: FloorId(1) })
        .unwrap();
    assert_eq!(w.revision(), before.next());
    assert!(w.floor(FloorId(1)).is_none());
    // Этаж, на который ведёт лестница, удалить нельзя.
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(1),
        name: "up".into(),
        base_y: 4.0,
        height: 3.0,
    }))
    .unwrap();
    w.apply(WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(50),
        kind: StructureKind::Stair { to: FloorId(1) },
        placement: Placement::Run {
            start: GridCell::new(5, 5),
            dir: astrocore_math::Facing::East,
            length: 3,
        },
        floor: FloorId(0),
        material: mat(),
        thickness: 0.2,
    }))
    .unwrap();
    assert!(matches!(
        w.apply(WorldCommand::RemoveFloor { id: FloorId(1) }),
        Err(WorldError::FloorNotEmpty(_))
    ));
}

mod freeform {
    use astrocore_math::glam::Vec3;

    use super::*;

    fn statue(id: u128, pos: [f32; 3], yaw: f32) -> StructureElement {
        StructureElement {
            id: ObjectId::from_u128(id),
            kind: StructureKind::Freeform,
            placement: Placement::Free(FreePlacement::from_position(
                pos,
                1.0,
                yaw,
                [0.5, 1.0, 0.25],
            )),
            floor: FloorId(0),
            material: PrototypeId::new("test:statue").unwrap(),
            thickness: 0.1,
        }
    }

    #[test]
    fn position_roundtrips_to_a_millimetre_and_negative_coordinates_work() {
        for pos in [[3.25, 0.5, 7.75], [-0.4, 1.0, -12.3], [100.001, 2.0, 0.0]] {
            let f = FreePlacement::from_position(pos, 1.0, 45.0, [0.5, 1.0, 0.5]);
            let back = f.position(1.0);
            for k in 0..3 {
                assert!((back[k] - pos[k]).abs() < 0.0011, "{pos:?} → {back:?}");
            }
            assert!(
                f.x_mm < 1000 && f.z_mm < 1000,
                "смещение внутри якорной ячейки"
            );
        }
        let f = FreePlacement::from_position([-0.4, 0.0, -0.4], 1.0, 0.0, [0.1; 3]);
        assert_eq!(f.cell, GridCell::new(-1, -1));
    }

    #[test]
    fn aabb_follows_yaw() {
        let at = |yaw| {
            FreePlacement::from_position([5.0, 1.0, 5.0], 1.0, yaw, [1.0, 0.5, 0.25]).aabb(1.0)
        };
        let (min, max) = at(0.0);
        assert!((max[0] - min[0] - 2.0).abs() < 1e-3 && (max[2] - min[2] - 0.5).abs() < 1e-3);
        let (min, max) = at(90.0);
        assert!(
            (max[0] - min[0] - 0.5).abs() < 1e-3 && (max[2] - min[2] - 2.0).abs() < 1e-3,
            "повёрнутый ящик «лёг» по другой оси"
        );
        assert!(
            (max[1] - min[1] - 1.0).abs() < 1e-5 && (min[1] - 1.0).abs() < 1e-5,
            "высота: от ног вверх"
        );
    }

    #[test]
    fn freeforms_do_not_occupy_slots_and_follow_the_usual_lifecycle() {
        let mut w = world();
        w.take_dirty();
        w.apply(WorldCommand::PlaceElement(statue(1, [3.5, 0.0, 3.5], 0.0)))
            .unwrap();
        w.apply(WorldCommand::PlaceElement(statue(2, [3.5, 0.0, 3.5], 30.0)))
            .unwrap(); // то же место — можно
        assert_eq!(w.element_count(), 2);
        let dirty = w.take_dirty();
        assert!(
            dirty.iter().all(|d| d.flags.contains(DirtyFlags::GEOMETRY)
                && d.flags.contains(DirtyFlags::COLLISION))
        );
        assert!(
            !dirty.iter().any(|d| d.flags.contains(DirtyFlags::TOPOLOGY)),
            "модель не меняет топологию"
        );
        assert_eq!(
            w.elements_in_chunk(ChunkCoord::new(0, 0), FloorId(0))
                .count(),
            2
        );
        let restored = World::from_state(w.to_state()).unwrap();
        assert_eq!(restored.content_hash(), w.content_hash());
        w.apply(WorldCommand::RemoveElement {
            id: ObjectId::from_u128(1),
        })
        .unwrap();
        assert_eq!(w.element_count(), 1);
    }

    #[test]
    fn invalid_freeforms_are_rejected() {
        let mut w = world();
        let mut e = statue(1, [3.5, 0.0, 3.5], 0.0);
        if let Placement::Free(f) = &mut e.placement {
            f.x_mm = 1500; // за пределами якорной ячейки
        }
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(e)),
            Err(WorldError::InvalidPlacement(_))
        ));
        let mut e = statue(2, [3.5, 0.0, 3.5], 0.0);
        if let Placement::Free(f) = &mut e.placement {
            f.half_mm = [0, 10, 10];
        }
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(e)),
            Err(WorldError::InvalidPlacement(_))
        ));
        let mut e = statue(3, [3.5, 0.0, 3.5], 0.0);
        if let Placement::Free(f) = &mut e.placement {
            f.half_mm = [200_000, 10, 10];
        }
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(e)),
            Err(WorldError::InvalidPlacement(_))
        ));
        // Свободная модель на ребре или стена со свободным размещением — неверное сочетание.
        let mut e = statue(4, [3.5, 0.0, 3.5], 0.0);
        e.placement = Placement::Edge(GridEdge::along_x(0, 0));
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(e)),
            Err(WorldError::InvalidPlacement(_))
        ));
        let mut e = statue(5, [3.5, 0.0, 3.5], 0.0);
        e.kind = StructureKind::Wall;
        assert!(matches!(
            w.apply(WorldCommand::PlaceElement(e)),
            Err(WorldError::InvalidPlacement(_))
        ));
        assert_eq!(w.element_count(), 0);
        let _ = Vec3::ZERO;
    }
}

mod authored {
    use astrocore_math::glam::Vec3;

    use crate::authored::MAX_METADATA_ENTRIES;

    use super::*;

    fn id(n: u128) -> ObjectId {
        ObjectId::from_u128(n)
    }

    fn volume(n: u128, lo: [f32; 3], hi: [f32; 3]) -> AuthoredObject {
        AuthoredObject::new(
            id(n),
            AuthoredKind::Volume,
            FloorId(0),
            Vec3::from(lo),
            Vec3::from(hi),
        )
    }

    fn portal(n: u128, to: u128, channels: u8) -> AuthoredObject {
        AuthoredObject::new(
            id(n),
            AuthoredKind::Portal {
                to: id(to),
                channels,
            },
            FloorId(0),
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(5.0, 2.0, 2.0),
        )
    }

    #[test]
    fn zones_volumes_and_portals_live_in_the_canonical_world() {
        let mut w = world();
        let r0 = w.revision();
        w.apply(WorldCommand::PlaceAuthored(volume(
            1,
            [0.0, 0.0, 0.0],
            [4.0, 3.0, 4.0],
        )))
        .unwrap();
        let mut zone = volume(2, [-10.0, 0.0, -10.0], [10.0, 4.0, 10.0]);
        zone.kind = AuthoredKind::Zone;
        zone.metadata.insert("name".into(), "двор".into());
        w.apply(WorldCommand::PlaceAuthored(zone)).unwrap();
        w.apply(WorldCommand::PlaceAuthored(portal(
            3,
            1,
            CHANNEL_PASS | CHANNEL_SIGHT,
        )))
        .unwrap();
        assert_eq!(w.revision().0, r0.0 + 3);
        assert_eq!(w.authored_count(), 3);
        let at: Vec<_> = w
            .authored_at(Vec3::new(1.0, 1.0, 1.0))
            .iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(at, vec![id(1), id(2)], "объём и зона содержат точку");
        assert_eq!(
            w.authored_at(Vec3::new(8.0, 1.0, 8.0)).len(),
            1,
            "только зона"
        );
        // Топология чанка помечена грязной.
        let dirty = w.take_dirty();
        assert!(dirty.iter().any(|d| d.flags.contains(DirtyFlags::TOPOLOGY)));
    }

    #[test]
    fn invalid_authored_objects_are_rejected_without_changing_the_world() {
        let mut w = world();
        let rev = w.revision();
        let mut cases: Vec<(AuthoredObject, &str)> = Vec::new();
        cases.push((volume(1, [0.0, 0.0, 0.0], [0.0, 3.0, 4.0]), "нулевой объём"));
        cases.push((
            portal(2, 99, CHANNEL_PASS),
            "портал на несуществующий объём",
        ));
        let mut far = volume(3, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        far.max_mm[0] = MAX_AUTHORED_MM + 1;
        cases.push((far, "слишком далеко"));
        let mut bad_floor = volume(4, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        bad_floor.floor = FloorId(7);
        cases.push((bad_floor, "нет этажа"));
        let mut meta = volume(5, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        for i in 0..=MAX_METADATA_ENTRIES {
            meta.metadata.insert(format!("k{i}"), "v".into());
        }
        cases.push((meta, "метаданные"));
        for (obj, why) in cases {
            assert!(w.apply(WorldCommand::PlaceAuthored(obj)).is_err(), "{why}");
        }
        assert_eq!(w.revision(), rev);
        // Точка размещения может быть нулевого размера.
        let spawn = AuthoredObject::new(
            id(6),
            AuthoredKind::Placement {
                prototype: PrototypeId::new("test:spawn").unwrap(),
            },
            FloorId(0),
            Vec3::new(3.0, 0.0, 3.0),
            Vec3::new(3.0, 0.0, 3.0),
        );
        w.apply(WorldCommand::PlaceAuthored(spawn)).unwrap();
        // Дубль идентификатора, в том числе со структурным элементом.
        assert!(matches!(
            w.apply(WorldCommand::PlaceAuthored(volume(6, [0.0; 3], [1.0; 3]))),
            Err(WorldError::DuplicateId(_))
        ));
        w.apply(WorldCommand::PlaceElement(wall(
            50,
            GridEdge::along_x(0, 0),
        )))
        .unwrap();
        assert!(matches!(
            w.apply(WorldCommand::PlaceAuthored(volume(50, [0.0; 3], [1.0; 3]))),
            Err(WorldError::DuplicateId(_))
        ));
    }

    #[test]
    fn referenced_volumes_and_used_floors_cannot_be_removed() {
        let mut w = world();
        w.apply(WorldCommand::PlaceAuthored(volume(
            1,
            [0.0; 3],
            [4.0, 3.0, 4.0],
        )))
        .unwrap();
        w.apply(WorldCommand::PlaceAuthored(portal(2, 1, CHANNEL_GAS)))
            .unwrap();
        assert!(w.apply(WorldCommand::RemoveAuthored { id: id(1) }).is_err());
        assert!(matches!(
            w.apply(WorldCommand::RemoveFloor { id: FloorId(0) }),
            Err(WorldError::FloorNotEmpty(_))
        ));
        w.apply(WorldCommand::RemoveAuthored { id: id(2) }).unwrap();
        w.apply(WorldCommand::RemoveAuthored { id: id(1) }).unwrap();
        assert_eq!(w.authored_count(), 0);
        assert!(w.apply(WorldCommand::RemoveAuthored { id: id(1) }).is_err());
    }

    #[test]
    fn state_round_trips_and_old_worlds_keep_their_hash() {
        let plain = world();
        // Старые снимки без поля `authored` читаются как пустой список с прежним хешем.
        let mut value = serde_json::to_value(plain.to_state()).unwrap();
        value.as_object_mut().unwrap().remove("authored");
        let old: WorldState = serde_json::from_value(value).unwrap();
        assert!(old.authored.is_empty());
        assert_eq!(
            World::from_state(old).unwrap().content_hash(),
            plain.content_hash()
        );
        let mut w = world();
        w.apply(WorldCommand::PlaceAuthored(volume(
            1,
            [0.0; 3],
            [4.0, 3.0, 4.0],
        )))
        .unwrap();
        w.apply(WorldCommand::PlaceAuthored(portal(2, 1, CHANNEL_PASS)))
            .unwrap();
        let back = World::from_state(w.to_state()).unwrap();
        assert_eq!(back.content_hash(), w.content_hash());
        assert_eq!(back.authored_count(), 2);
        // Повреждённое состояние (портал на неизвестный объём) не загружается.
        let mut state = w.to_state();
        state.authored.retain(|o| o.id != id(1));
        assert!(World::from_state(state).is_err());
    }

    #[test]
    fn authored_operations_replicate_idempotently() {
        let mut server = world();
        let mut client = server.clone();
        let ops = vec![
            server
                .apply(WorldCommand::PlaceAuthored(volume(
                    1,
                    [0.0; 3],
                    [4.0, 3.0, 4.0],
                )))
                .unwrap(),
            server
                .apply(WorldCommand::PlaceAuthored(portal(2, 1, CHANNEL_PASS)))
                .unwrap(),
        ];
        for op in &ops {
            assert_eq!(client.apply_replicated(op), ReplicaOutcome::Applied);
        }
        for op in &ops {
            assert_eq!(client.apply_replicated(op), ReplicaOutcome::Duplicate);
        }
        assert_eq!(client.content_hash(), server.content_hash());
    }
}
