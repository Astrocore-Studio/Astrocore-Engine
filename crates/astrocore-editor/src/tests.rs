use std::collections::BTreeSet;

use astrocore_core::{ObjectId, PrefabInstanceId, PrefabNodeId, PrototypeId};
use astrocore_math::glam::Vec3;
use astrocore_math::{Facing, GridCell, GridEdge, GridTransform};
use astrocore_network::testkit::Harness;
use astrocore_prefabs::{
    ChildRef, ElementTemplate, LocalKind, LocalPlacement, NodeContent, PrefabDef, PrefabInstance,
    PrefabNode, PrefabRegistry,
};
use astrocore_spatial::Topology;
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, SlotKey, StructureKind, World, WorldCommand,
    WorldConfig,
};

use crate::playtest::{PlaytestConfig, start_playtest};
use crate::project::{InstanceRecord, Project, ProjectError};
use crate::session::{EditError, EditSession};
use crate::tools::{self, SequentialIds, ToolError};
use crate::validate::{FindingKind, Severity, has_errors, validate_map};
use crate::view::{FloorView, Visibility, visibility, visible_floors};

fn mat() -> PrototypeId {
    PrototypeId::new("t:stone").unwrap()
}

fn base_world(floors: i32) -> World {
    let mut w = World::new("ed", WorldConfig::default()).unwrap();
    for f in 0..floors {
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(f),
            name: format!("f{f}"),
            base_y: f as f32 * 4.0,
            height: 4.0,
        }))
        .unwrap();
    }
    w
}

fn session(floors: i32) -> EditSession {
    EditSession::new(base_world(floors))
}

#[test]
fn execute_undo_redo_roundtrip_restores_the_exact_world() {
    let mut s = session(1);
    let empty = s.world().content_hash();
    let mut ids = SequentialIds(0);
    let cmds = tools::room_rect(
        s.world(),
        &mut ids,
        FloorId(0),
        GridCell::new(0, 0),
        GridCell::new(3, 2),
        &mat(),
        0.2,
        true,
        true,
    )
    .unwrap();
    // 2*(4+3) стен + 12 пол + 12 потолок.
    assert_eq!(cmds.len(), 14 + 12 + 12);
    s.execute("Комната", cmds).unwrap();
    let built = s.world().content_hash();
    assert_ne!(built, empty);

    assert_eq!(s.undo_label(), Some("Комната"));
    assert_eq!(s.undo().unwrap(), "Комната");
    assert_eq!(s.world().element_count(), 0);
    assert!(s.can_redo() && !s.can_undo());
    assert_eq!(s.redo().unwrap(), "Комната");
    assert_eq!(s.world().element_count(), 14 + 24);
    // Содержимое (без учёта ревизий) совпадает с тем, что было после первой сборки.
    let strip = |w: &World| {
        let mut st = w.to_state();
        st.revision = astrocore_core::Revision(0);
        st.chunk_revisions.clear();
        serde_json::to_string(&st).unwrap()
    };
    let mut again = session(1);
    again
        .execute(
            "x",
            tools::room_rect(
                again.world(),
                &mut SequentialIds(0),
                FloorId(0),
                GridCell::new(0, 0),
                GridCell::new(3, 2),
                &mat(),
                0.2,
                true,
                true,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(strip(s.world()), strip(again.world()));
    let _ = built;
}

#[test]
fn a_new_edit_clears_the_redo_stack_and_empty_transactions_are_not_recorded() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "a",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (3, 0)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    s.undo().unwrap();
    assert!(s.can_redo());
    s.execute(
        "b",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (1, 0)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!s.can_redo(), "новая правка сбрасывает «повторить»");
    let depth = s.history_len();
    s.execute("пусто", vec![]).unwrap();
    assert_eq!(s.history_len(), depth);
    assert_eq!(s.redo(), Err(EditError::NothingToRedo));
    let mut fresh = session(1);
    assert_eq!(fresh.undo(), Err(EditError::NothingToUndo));
}

#[test]
fn a_failing_transaction_rolls_back_completely() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "стена",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (1, 0)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    let before = s.world().to_state().elements;
    let mut cmds = tools::wall_path(
        s.world(),
        &mut ids,
        FloorId(0),
        &[(5, 5), (7, 5)],
        &mat(),
        0.2,
    )
    .unwrap();
    cmds.push(WorldCommand::RemoveElement {
        id: ObjectId::from_u128(404),
    }); // не существует — транзакция упадёт
    let depth = s.history_len();
    assert!(matches!(
        s.execute("плохая", cmds),
        Err(EditError::World(_))
    ));
    assert_eq!(
        s.world().to_state().elements,
        before,
        "частично применённые команды откатились"
    );
    assert_eq!(s.history_len(), depth);
    // Сессия остаётся рабочей.
    s.undo().unwrap();
}

#[test]
fn history_depth_is_bounded() {
    let mut s = EditSession::new(base_world(1)).with_depth(3);
    let mut ids = SequentialIds(0);
    for i in 0..6 {
        s.execute(
            format!("шаг {i}"),
            tools::wall_path(
                s.world(),
                &mut ids,
                FloorId(0),
                &[(i * 2, 0), (i * 2 + 1, 0)],
                &mat(),
                0.2,
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(s.history_len(), 3);
    for _ in 0..3 {
        s.undo().unwrap();
    }
    assert_eq!(s.undo(), Err(EditError::NothingToUndo));
    assert_eq!(
        s.world().element_count(),
        3,
        "старые правки остались, потому что история их забыла"
    );
}

#[test]
fn undo_after_replace_material_restores_the_old_material_with_same_id() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "стены",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (3, 0)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    let sel: BTreeSet<ObjectId> = s.world().elements().map(|e| e.id).collect();
    let wood = PrototypeId::new("t:wood").unwrap();
    s.execute(
        "материал",
        tools::replace_material(s.world(), &sel, &wood).unwrap(),
    )
    .unwrap();
    assert!(s.world().elements().all(|e| e.material == wood));
    assert_eq!(
        s.world().elements().map(|e| e.id).collect::<BTreeSet<_>>(),
        sel,
        "идентичность сохранилась"
    );
    s.undo().unwrap();
    assert!(s.world().elements().all(|e| e.material == mat()));
    assert_eq!(
        tools::replace_material(s.world(), &sel, &mat()),
        Err(ToolError::Empty),
        "ничего не меняется"
    );
}

#[test]
fn external_changes_invalidate_the_history() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "a",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (1, 0)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    s.apply_untracked(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(9),
        name: "x".into(),
        base_y: 40.0,
        height: 3.0,
    }))
    .unwrap();
    assert!(!s.can_undo(), "после внешней правки история сброшена");
}

#[test]
fn journal_replays_to_the_same_world() {
    let mut s = session(2);
    let mut ids = SequentialIds(0);
    s.execute(
        "а",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(2, 2),
            &mat(),
            0.2,
            true,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    s.undo().unwrap();
    s.redo().unwrap();
    // Журнал сессии, применённый к чистому миру с теми же этажами, даёт ту же карту.
    let mut replay = World::new("ed", WorldConfig::default()).unwrap();
    // Журнал содержит и определения этажей из базового мира? Нет — они созданы до сессии: добавим их вручную.
    for f in 0..2 {
        replay
            .apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(f),
                name: format!("f{f}"),
                base_y: f as f32 * 4.0,
                height: 4.0,
            }))
            .unwrap();
    }
    for op in s.journal() {
        replay.apply(op.command.clone()).unwrap();
    }
    assert_eq!(replay.content_hash(), s.world().content_hash());
}

#[test]
fn wall_path_validates_and_is_idempotent() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    assert!(matches!(
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (3, 2)],
            &mat(),
            0.2
        ),
        Err(ToolError::NotAxisAligned(0, 0, 3, 2))
    ));
    assert!(matches!(
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(7),
            &[(0, 0), (1, 0)],
            &mat(),
            0.2
        ),
        Err(ToolError::UnknownFloor(_))
    ));
    let cmds = tools::wall_path(
        s.world(),
        &mut ids,
        FloorId(0),
        &[(0, 0), (4, 0), (4, 3), (4, 0)],
        &mat(),
        0.2,
    )
    .unwrap();
    assert_eq!(
        cmds.len(),
        4 + 3,
        "обратный проход по тем же рёбрам не дублирует стены"
    );
    s.execute("линия", cmds).unwrap();
    let again = tools::wall_path(
        s.world(),
        &mut ids,
        FloorId(0),
        &[(0, 0), (4, 0)],
        &mat(),
        0.2,
    )
    .unwrap();
    assert!(again.is_empty(), "существующие стены пропускаются");
    // Проём не заменяется стеной молча.
    let door = WorldCommand::PlaceElement(astrocore_world::StructureElement {
        id: ObjectId::from_u128(900),
        kind: StructureKind::Opening(OpeningKind::Door),
        placement: Placement::Edge(GridEdge::along_x(10, 0)),
        floor: FloorId(0),
        material: mat(),
        thickness: 0.1,
    });
    s.execute("дверь", vec![door]).unwrap();
    assert!(matches!(
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(9, 0), (12, 0)],
            &mat(),
            0.2
        ),
        Err(ToolError::EdgeHasOpening(_))
    ));
}

#[test]
fn closing_a_contour_then_auto_slabs_fill_the_room() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    // Только стены — без пола и потолка.
    s.execute(
        "стены",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(2, 2),
            GridCell::new(5, 4),
            &mat(),
            0.2,
            false,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(s.world().element_count(), 14);
    s.execute(
        "пол",
        tools::auto_slabs(
            s.world(),
            &mut ids,
            FloorId(0),
            StructureKind::Floor,
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    s.execute(
        "потолок",
        tools::auto_slabs(
            s.world(),
            &mut ids,
            FloorId(0),
            StructureKind::Ceiling,
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(s.world().element_count(), 14 + 12 + 12);
    for x in 2..=5 {
        for z in 2..=4 {
            let c = GridCell::new(x, z);
            assert!(
                s.world()
                    .element_at(FloorId(0), SlotKey::FloorSlab(c))
                    .is_some(),
                "{c:?}"
            );
        }
    }
    // Снаружи пол не появился.
    assert!(
        s.world()
            .element_at(FloorId(0), SlotKey::FloorSlab(GridCell::new(1, 2)))
            .is_none()
    );
    // Повторный вызов — нечего делать.
    assert_eq!(
        tools::auto_slabs(
            s.world(),
            &mut ids,
            FloorId(0),
            StructureKind::Floor,
            &mat(),
            0.2
        ),
        Err(ToolError::Empty)
    );
    // Незамкнутый контур не даёт помещения.
    let mut open = session(1);
    open.execute(
        "г",
        tools::wall_path(
            open.world(),
            &mut ids,
            FloorId(0),
            &[(0, 0), (4, 0), (4, 4)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        tools::auto_slabs(
            open.world(),
            &mut ids,
            FloorId(0),
            StructureKind::Floor,
            &mat(),
            0.2
        ),
        Err(ToolError::Empty)
    );
    assert!(matches!(
        tools::auto_slabs(
            open.world(),
            &mut ids,
            FloorId(0),
            StructureKind::Wall,
            &mat(),
            0.2
        ),
        Err(ToolError::Occupied(_))
    ));
}

#[test]
fn stair_tool_pierces_floors_and_the_result_validates() {
    let mut s = session(2);
    let mut ids = SequentialIds(0);
    // Нижний этаж: комната с полом и потолком; верхний: пол над всей комнатой + площадка за ней.
    s.execute(
        "низ",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(7, 2),
            &mat(),
            0.2,
            true,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let mut upper = Vec::new();
    for x in 0..=9 {
        for z in 0..=2 {
            upper.push(WorldCommand::PlaceElement(
                astrocore_world::StructureElement {
                    id: ObjectId::from_u128(10_000 + (x * 10 + z) as u128),
                    kind: StructureKind::Floor,
                    placement: Placement::Cell(GridCell::new(x, z)),
                    floor: FloorId(1),
                    material: mat(),
                    thickness: 0.2,
                },
            ));
        }
    }
    s.execute("верх", upper).unwrap();
    // Лестница вдоль +X от (1,1), длина 4: клетки x = 1..4, выход (5,1) на верхнем этаже.
    let cmds = tools::stair_between(
        s.world(),
        &mut ids,
        FloorId(0),
        FloorId(1),
        GridCell::new(1, 1),
        Facing::East,
        4,
        false,
        &mat(),
        true,
    )
    .unwrap();
    assert_eq!(
        cmds.len(),
        1 + 4 + 4,
        "лестница + 4 клетки пола наверху + 4 клетки потолка внизу"
    );
    s.execute("лестница", cmds).unwrap();
    for x in 1..=4 {
        assert!(
            s.world()
                .element_at(FloorId(1), SlotKey::FloorSlab(GridCell::new(x, 1)))
                .is_none()
        );
        assert!(
            s.world()
                .element_at(FloorId(0), SlotKey::CeilingSlab(GridCell::new(x, 1)))
                .is_none()
        );
    }
    assert!(
        s.world()
            .element_at(FloorId(1), SlotKey::FloorSlab(GridCell::new(5, 1)))
            .is_some(),
        "площадка осталась"
    );
    assert_eq!(s.world().vertical_links().len(), 1);

    let findings = validate_map(s.world());
    assert!(!has_errors(&findings), "{findings:?}");
    // Единая транзакция отменяется целиком.
    s.undo().unwrap();
    assert_eq!(s.world().vertical_links().len(), 0);
    assert!(
        s.world()
            .element_at(FloorId(1), SlotKey::FloorSlab(GridCell::new(2, 1)))
            .is_some(),
        "перекрытие вернулось"
    );

    // Занятая клетка отклоняется до применения.
    s.redo().unwrap();
    assert!(matches!(
        tools::stair_between(
            s.world(),
            &mut ids,
            FloorId(0),
            FloorId(1),
            GridCell::new(3, 1),
            Facing::East,
            2,
            false,
            &mat(),
            true
        ),
        Err(ToolError::Occupied(_))
    ));
    assert!(matches!(
        tools::stair_between(
            s.world(),
            &mut ids,
            FloorId(0),
            FloorId(5),
            GridCell::new(1, 2),
            Facing::East,
            2,
            false,
            &mat(),
            true
        ),
        Err(ToolError::UnknownFloor(_))
    ));
}

#[test]
fn validator_reports_problems_with_severity() {
    let mut s = session(2);
    let mut ids = SequentialIds(0);
    s.execute(
        "комната",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(3, 3),
            &mat(),
            0.2,
            true,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let kinds: Vec<_> = validate_map(s.world())
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, FindingKind::SealedRoom { .. })),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&FindingKind::EmptyFloor),
        "верхний этаж пуст"
    );

    // Лестница без перекрытия-площадки наверху и с оставленным перекрытием над собой.
    let mut upper = Vec::new();
    for x in 0..=3 {
        upper.push(WorldCommand::PlaceElement(
            astrocore_world::StructureElement {
                id: ObjectId::from_u128(20_000 + x as u128),
                kind: StructureKind::Floor,
                placement: Placement::Cell(GridCell::new(x, 1)),
                floor: FloorId(1),
                material: mat(),
                thickness: 0.2,
            },
        ));
    }
    s.execute("верх", upper).unwrap();
    let no_pierce = tools::stair_between(
        s.world(),
        &mut ids,
        FloorId(0),
        FloorId(1),
        GridCell::new(0, 1),
        Facing::East,
        3,
        false,
        &mat(),
        false,
    )
    .unwrap();
    s.execute("без пробивки", no_pierce).unwrap();
    let findings = validate_map(s.world());
    assert!(has_errors(&findings));
    assert!(findings.iter().any(
        |f| matches!(f.kind, FindingKind::StairBlockedByFloor { .. })
            && f.severity == Severity::Error
    ));
    assert!(
        findings
            .first()
            .is_some_and(|f| f.severity == Severity::Error),
        "ошибки идут первыми"
    );
}

#[test]
fn copy_rotate_and_mirror_selection() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "комната",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(2, 1),
            &mat(),
            0.2,
            true,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let sel = tools::select_rect(
        s.world(),
        FloorId(0),
        GridCell::new(0, 0),
        GridCell::new(2, 1),
    );
    assert_eq!(sel.len(), 10 + 6);

    // Копия, перенесённая на 10 клеток: структура та же, идентификаторы новые.
    let cmds = tools::copy_selection(
        s.world(),
        &mut ids,
        &sel,
        GridTransform::translate(10, 0),
        0,
    )
    .unwrap();
    s.execute("копия", cmds).unwrap();
    assert_eq!(s.world().element_count(), 32);
    let topo = Topology::build(s.world(), FloorId(0)).unwrap();
    assert_eq!(topo.interior_rooms().count(), 2, "обе комнаты замкнуты");

    // Поворот копии на месте нельзя — клетки заняты; в стороне можно.
    assert!(matches!(
        tools::copy_selection(s.world(), &mut ids, &sel, GridTransform::IDENTITY, 0),
        Err(ToolError::Occupied(_))
    ));
    let rot = GridTransform {
        mirror_x: false,
        quarter_turns: 1,
        dx: 30,
        dz: 30,
    };
    s.execute(
        "поворот",
        tools::copy_selection(s.world(), &mut ids, &sel, rot, 0).unwrap(),
    )
    .unwrap();
    let topo = Topology::build(s.world(), FloorId(0)).unwrap();
    assert_eq!(topo.interior_rooms().count(), 3);
    let rotated_room_cells = topo.interior_rooms().map(|r| r.cells.len()).max().unwrap();
    assert_eq!(
        rotated_room_cells, 6,
        "повёрнутая комната 3×2 осталась из 6 клеток"
    );

    // Зеркало.
    let mirror = GridTransform {
        mirror_x: true,
        quarter_turns: 0,
        dx: 60,
        dz: 0,
    };
    s.execute(
        "зеркало",
        tools::copy_selection(s.world(), &mut ids, &sel, mirror, 0).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Topology::build(s.world(), FloorId(0))
            .unwrap()
            .interior_rooms()
            .count(),
        4
    );
    assert_eq!(
        tools::copy_selection(
            s.world(),
            &mut ids,
            &BTreeSet::new(),
            GridTransform::IDENTITY,
            0
        ),
        Err(ToolError::EmptySelection)
    );
}

#[test]
fn moving_a_selection_keeps_object_ids() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "комната",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(1, 1),
            &mat(),
            0.2,
            true,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let sel = tools::select_rect(
        s.world(),
        FloorId(0),
        GridCell::new(0, 0),
        GridCell::new(1, 1),
    );
    let before_ids: BTreeSet<ObjectId> = s.world().elements().map(|e| e.id).collect();
    // Сдвиг на одну клетку вправо пересекается с собой, но выделение освобождает свои места.
    s.execute(
        "сдвиг",
        tools::move_selection(s.world(), &sel, GridTransform::translate(1, 0), 0).unwrap(),
    )
    .unwrap();
    let after_ids: BTreeSet<ObjectId> = s.world().elements().map(|e| e.id).collect();
    assert_eq!(before_ids, after_ids);
    assert!(
        s.world()
            .element_at(FloorId(0), SlotKey::FloorSlab(GridCell::new(2, 0)))
            .is_some()
    );
    assert!(
        s.world()
            .element_at(FloorId(0), SlotKey::FloorSlab(GridCell::new(0, 0)))
            .is_none()
    );
    s.undo().unwrap();
    assert!(
        s.world()
            .element_at(FloorId(0), SlotKey::FloorSlab(GridCell::new(0, 0)))
            .is_some()
    );
}

#[test]
fn floors_can_be_added_and_undone() {
    let mut s = session(1);
    let (id, cmd) = tools::add_floor_above(s.world(), "верхний", 3.0);
    assert_eq!(id, FloorId(1));
    s.execute("этаж", vec![cmd]).unwrap();
    assert!((s.world().floor(FloorId(1)).unwrap().base_y - 4.0).abs() < 1e-6);
    s.undo().unwrap();
    assert!(s.world().floor(FloorId(1)).is_none());
    s.redo().unwrap();
    assert!(s.world().floor(FloorId(1)).is_some());
}

fn room_prefab() -> PrefabDef {
    let mut d = PrefabDef::new(PrototypeId::new("t:room").unwrap());
    let mut n = 1u128;
    for i in 0..2 {
        for e in [
            GridEdge::along_x(i, 0),
            GridEdge::along_x(i, 2),
            GridEdge::along_z(0, i),
            GridEdge::along_z(2, i),
        ] {
            d.nodes.push(PrefabNode {
                id: PrefabNodeId::from_u128(n),
                name: format!("w{n}"),
                content: NodeContent::Element(ElementTemplate {
                    kind: LocalKind::Wall,
                    placement: LocalPlacement::Edge(e),
                    floor_offset: 0,
                    material: mat(),
                    thickness: 0.2,
                }),
            });
            n += 1;
        }
    }
    d
}

#[test]
fn prefab_placement_updates_and_project_roundtrip() {
    let mut reg = PrefabRegistry::new();
    reg.insert(room_prefab()).unwrap();
    let mut tower = PrefabDef::new(PrototypeId::new("t:tower").unwrap());
    tower.nodes.push(PrefabNode {
        id: PrefabNodeId::from_u128(99),
        name: "комната".into(),
        content: NodeContent::Child(ChildRef {
            prefab: PrototypeId::new("t:room").unwrap(),
            required_version: Some(1),
            transform: GridTransform::translate(5, 5),
            floor_offset: 0,
        }),
    });
    reg.insert(tower).unwrap();
    reg.validate().unwrap();

    let mut s = session(1);
    let inst = PrefabInstance::new(
        PrefabInstanceId::from_u128(7),
        PrototypeId::new("t:tower").unwrap(),
    );
    let plan = tools::place_prefab(&reg, &inst, &BTreeSet::new(), s.world()).unwrap();
    assert_eq!((plan.commands.len(), plan.conflicts.len()), (8, 0));
    s.execute("башня", plan.commands).unwrap();
    // Проект сохраняется и открывается с теми же объектами и записью экземпляра.
    let dir = std::env::temp_dir().join(format!("astrocore-editor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("map.acproj");
    let mut project = Project::new("тест", s.world().clone());
    project.prefabs = reg;
    project.instances.push(InstanceRecord {
        instance: inst.clone(),
        spawned: plan.spawned.clone(),
    });
    project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.world.content_hash(), s.world().content_hash());
    assert_eq!(
        loaded.instances[0].spawned, plan.spawned,
        "стабильные ObjectId сохранились"
    );

    // Обновление префаба: исходник теряет один узел, обновление экземпляра убирает его объект.
    let mut reg2 = loaded.prefabs.clone();
    let mut v2 = room_prefab();
    v2.version = 2;
    v2.nodes.pop();
    reg2.replace(v2).unwrap();
    let mut tower2 = PrefabDef::new(PrototypeId::new("t:tower").unwrap());
    tower2.nodes.push(PrefabNode {
        id: PrefabNodeId::from_u128(99),
        name: "комната".into(),
        content: NodeContent::Child(ChildRef {
            prefab: PrototypeId::new("t:room").unwrap(),
            required_version: Some(2),
            transform: GridTransform::translate(5, 5),
            floor_offset: 0,
        }),
    });
    reg2.replace(tower2).unwrap();
    let upd = tools::place_prefab(&reg2, &inst, &plan.spawned, s.world()).unwrap();
    assert_eq!(upd.commands.len(), 1, "удалён ровно один объект");
    s.execute("обновить", upd.commands).unwrap();
    assert_eq!(s.world().element_count(), 7);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn hostile_project_files_are_rejected() {
    let dir = std::env::temp_dir().join(format!("astrocore-editor-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: &str| {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    };
    assert!(matches!(
        Project::load(&write("a", "{ nope")),
        Err(ProjectError::Json(_))
    ));
    let good = Project::new("x", base_world(1)).to_file();
    let mut wrong = good.clone();
    wrong.format_version = 9;
    assert!(matches!(
        Project::load(&write("b", &serde_json::to_string(&wrong).unwrap())),
        Err(ProjectError::Version(9))
    ));
    let mut bad_world = good.clone();
    bad_world.world.config.grid_step_m = 0.0;
    assert!(matches!(
        Project::load(&write("c", &serde_json::to_string(&bad_world).unwrap())),
        Err(ProjectError::World(_))
    ));
    let mut dangling = good;
    dangling.instances.push(InstanceRecord {
        instance: PrefabInstance::new(
            PrefabInstanceId::from_u128(1),
            PrototypeId::new("t:ghost").unwrap(),
        ),
        spawned: BTreeSet::new(),
    });
    assert!(matches!(
        Project::load(&write("d", &serde_json::to_string(&dangling).unwrap())),
        Err(ProjectError::UnknownPrefab(_))
    ));
    assert!(matches!(
        Project::load(&dir.join("missing")),
        Err(ProjectError::Io(_))
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn floor_views_hide_upper_floors_without_touching_the_world() {
    let mut s = session(3);
    let mut ids = SequentialIds(0);
    for f in 0..3 {
        s.execute(
            "этаж",
            tools::room_rect(
                s.world(),
                &mut ids,
                FloorId(f),
                GridCell::new(0, 0),
                GridCell::new(1, 1),
                &mat(),
                0.2,
                true,
                true,
            )
            .unwrap(),
        )
        .unwrap();
    }
    let hash = s.world().content_hash();
    let world = s.world();
    let slice = FloorView::Slice { top: FloorId(1) };
    assert_eq!(
        visible_floors(world, slice),
        BTreeSet::from([FloorId(0), FloorId(1)])
    );
    let vis = |kind: StructureKind, floor: i32| {
        let e = world
            .elements()
            .find(|e| {
                e.floor == FloorId(floor)
                    && std::mem::discriminant(&e.kind) == std::mem::discriminant(&kind)
            })
            .unwrap();
        visibility(world, slice, e)
    };
    assert_eq!(
        vis(StructureKind::Ceiling, 1),
        Visibility::Hidden,
        "потолок среза скрыт, чтобы видеть внутрь"
    );
    assert_eq!(vis(StructureKind::Wall, 1), Visibility::Opaque);
    assert_eq!(vis(StructureKind::Ceiling, 0), Visibility::Opaque);
    assert_eq!(vis(StructureKind::Wall, 2), Visibility::Hidden);
    let xray = FloorView::XRay { focus: FloorId(2) };
    assert_eq!(visible_floors(world, xray).len(), 3);
    let e0 = world.elements().find(|e| e.floor == FloorId(0)).unwrap();
    let e2 = world.elements().find(|e| e.floor == FloorId(2)).unwrap();
    assert_eq!(visibility(world, xray, e0), Visibility::Translucent);
    assert_eq!(visibility(world, xray, e2), Visibility::Opaque);
    assert_eq!(visibility(world, FloorView::All, e0), Visibility::Opaque);
    assert_eq!(world.content_hash(), hash, "режим просмотра мир не меняет");
    assert!(visible_floors(world, FloorView::Slice { top: FloorId(9) }).is_empty());
}

#[test]
fn playtest_runs_the_real_server_on_a_copy_of_the_map() {
    let mut s = session(1);
    let mut ids = SequentialIds(0);
    s.execute(
        "комната",
        tools::room_rect(
            s.world(),
            &mut ids,
            FloorId(0),
            GridCell::new(0, 0),
            GridCell::new(5, 5),
            &mat(),
            0.2,
            true,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let cfg = PlaytestConfig {
        floor: FloorId(0),
        spawn_cell: GridCell::new(2, 2),
        tick_hz: 20,
    };
    let server = start_playtest(s.world(), &cfg).expect("сервер запущен");
    let mut h = Harness::with_server(server);
    let player = h.join("tester");
    assert!(h.client(player).is_ready());
    assert_eq!(
        h.client(player).world().unwrap().content_hash(),
        s.world().content_hash()
    );
    assert!((h.client(player).position() - Vec3::new(2.5, 1.0, 2.5)).length() < 1e-5);
    // Игрок падает на пол и ходит внутри комнаты, упираясь в стену.
    for _ in 0..80 {
        h.input(player, 1, 0, 0.0);
    }
    let pos = h.server.player_position(player).unwrap();
    assert!(
        pos.x < 6.0 && pos.x > 5.0,
        "упёрся в восточную стену: {pos}"
    );
    assert!((pos.y - 0.2).abs() < 0.05, "стоит на полу: {pos}");
    // Правка карты в редакторе после пробной игры не влияет на уже запущенный сервер (копия).
    s.execute(
        "ещё стена",
        tools::wall_path(
            s.world(),
            &mut ids,
            FloorId(0),
            &[(8, 8), (9, 8)],
            &mat(),
            0.2,
        )
        .unwrap(),
    )
    .unwrap();
    assert_ne!(h.server.world().content_hash(), s.world().content_hash());
    assert!(
        start_playtest(
            s.world(),
            &PlaytestConfig {
                floor: FloorId(7),
                ..cfg
            }
        )
        .is_none()
    );
}
