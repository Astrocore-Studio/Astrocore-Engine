use std::collections::BTreeSet;

use astrocore_core::{ObjectId, PrefabInstanceId, PrefabNodeId, PrototypeId};
use astrocore_math::{Facing, GridCell, GridEdge, GridTransform};
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, StructureKind, World, WorldCommand, WorldConfig,
};

use crate::*;

fn pid(s: &str) -> PrototypeId {
    PrototypeId::new(s).unwrap()
}

fn nid(n: u128) -> PrefabNodeId {
    PrefabNodeId::from_u128(n)
}

fn wall_node(n: u128, edge: GridEdge, mat: &str) -> PrefabNode {
    PrefabNode {
        id: nid(n),
        name: format!("wall-{n}"),
        content: NodeContent::Element(ElementTemplate {
            kind: LocalKind::Wall,
            placement: LocalPlacement::Edge(edge),
            floor_offset: 0,
            material: pid(mat),
            thickness: 0.2,
        }),
    }
}

fn child_node(
    n: u128,
    prefab: &str,
    transform: GridTransform,
    floor_offset: i32,
    required: Option<u32>,
) -> PrefabNode {
    PrefabNode {
        id: nid(n),
        name: format!("child-{n}"),
        content: NodeContent::Child(ChildRef {
            prefab: pid(prefab),
            required_version: required,
            transform,
            floor_offset,
        }),
    }
}

/// Комната 2×2: 8 стен по периметру.
fn room_def() -> PrefabDef {
    let mut d = PrefabDef::new(pid("t:room"));
    let mut n = 1;
    for i in 0..2 {
        for e in [
            GridEdge::along_x(i, 0),
            GridEdge::along_x(i, 2),
            GridEdge::along_z(0, i),
            GridEdge::along_z(2, i),
        ] {
            d.nodes.push(wall_node(n, e, "t:stone"));
            n += 1;
        }
    }
    d
}

fn reg_with_room() -> PrefabRegistry {
    let mut r = PrefabRegistry::new();
    r.insert(room_def()).unwrap();
    r
}

fn world_with_floors(n: i32) -> World {
    let mut w = World::new("t", WorldConfig::default()).unwrap();
    for f in 0..n {
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

fn instance(prefab: &str, n: u128) -> PrefabInstance {
    PrefabInstance::new(PrefabInstanceId::from_u128(n), pid(prefab))
}

fn apply_all(w: &mut World, cmds: &[WorldCommand]) {
    for c in cmds {
        w.apply(c.clone()).unwrap_or_else(|e| panic!("{c:?}: {e}"));
    }
}

#[test]
fn instance_materializes_with_stable_derived_ids() {
    let reg = reg_with_room();
    let inst = instance("t:room", 100);
    let a = materialize(&reg, &inst).unwrap();
    let b = materialize(&reg, &inst).unwrap();
    assert_eq!(a.len(), 8);
    assert_eq!(a, b, "тот же экземпляр — те же ID");
    let other = materialize(&reg, &instance("t:room", 101)).unwrap();
    assert!(
        a.keys().all(|id| !other.contains_key(id)),
        "у разных экземпляров ID различаются"
    );
    assert!(a.contains_key(&object_id_for(inst.id, &[nid(1)])));
}

#[test]
fn ids_do_not_depend_on_node_order_or_position() {
    let mut reg = reg_with_room();
    let inst = instance("t:room", 1);
    let before = materialize(&reg, &inst).unwrap();
    let mut def = room_def();
    def.nodes.reverse();
    reg.replace(def).unwrap();
    let mut moved = instance("t:room", 1);
    moved.transform = GridTransform::translate(10, -4);
    let after = materialize(&reg, &moved).unwrap();
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
}

#[test]
fn transform_rotates_and_mirrors_placements() {
    let reg = reg_with_room();
    let mut inst = instance("t:room", 1);
    inst.transform = GridTransform {
        mirror_x: false,
        quarter_turns: 1,
        dx: 5,
        dz: 5,
    };
    let els = materialize(&reg, &inst).unwrap();
    // Все стены остаются периметром квадрата 2×2: каждое ребро граничит с одной внутренней ячейкой.
    let inner: BTreeSet<GridCell> = (0..2)
        .flat_map(|x| (0..2).map(move |z| inst.transform.cell(GridCell::new(x, z))))
        .collect();
    for e in els.values() {
        let Placement::Edge(edge) = e.placement else {
            panic!()
        };
        let inside = edge
            .adjacent_cells()
            .iter()
            .filter(|c| inner.contains(c))
            .count();
        assert_eq!(inside, 1, "{edge:?}");
    }
}

#[test]
fn nesting_applies_transforms_and_floor_offsets() {
    let mut reg = reg_with_room();
    let mut tower = PrefabDef::new(pid("t:tower"));
    tower
        .nodes
        .push(child_node(1, "t:room", GridTransform::IDENTITY, 0, None));
    tower.nodes.push(child_node(
        2,
        "t:room",
        GridTransform::translate(0, 0),
        2,
        Some(1),
    ));
    reg.insert(tower).unwrap();
    reg.validate().unwrap();
    let mut inst = instance("t:tower", 7);
    inst.base_floor = 1;
    let els = materialize(&reg, &inst).unwrap();
    assert_eq!(els.len(), 16);
    let floors: BTreeSet<_> = els.values().map(|e| e.floor).collect();
    assert_eq!(floors, BTreeSet::from([FloorId(1), FloorId(3)]));
    let paths = resolve(&reg, &pid("t:tower")).unwrap();
    assert!(
        paths.iter().all(|n| n.path.len() == 2),
        "путь = [дочерний узел, узел комнаты]"
    );
}

#[test]
fn inheritance_creates_variants_with_overrides() {
    let mut reg = reg_with_room();
    let mut wood = PrefabDef::new(pid("t:room_wood"));
    wood.parent = Some(pid("t:room"));
    wood.overrides.insert(
        nid(1),
        NodeOverride {
            material: Some(pid("t:wood")),
            ..Default::default()
        },
    );
    wood.overrides.insert(
        nid(2),
        NodeOverride {
            removed: true,
            ..Default::default()
        },
    );
    wood.nodes.push(PrefabNode {
        id: nid(100),
        name: "door".into(),
        content: NodeContent::Element(ElementTemplate {
            kind: LocalKind::Opening(OpeningKind::Door),
            // Место удалённой стены 2: дверь заменяет её.
            placement: LocalPlacement::Edge(GridEdge::along_x(0, 2)),
            floor_offset: 0,
            material: pid("t:door"),
            thickness: 0.2,
        }),
    });
    // Собственный узел с тем же ID заменяет родительский: стена 3 → окно.
    wood.nodes.push(PrefabNode {
        id: nid(3),
        name: "window".into(),
        content: NodeContent::Element(ElementTemplate {
            kind: LocalKind::Opening(OpeningKind::Window),
            placement: LocalPlacement::Edge(GridEdge::along_z(0, 0)),
            floor_offset: 0,
            material: pid("t:glass"),
            thickness: 0.1,
        }),
    });
    reg.insert(wood).unwrap();
    reg.validate().unwrap();

    let els = materialize(&reg, &instance("t:room_wood", 1)).unwrap();
    // 8 родительских − 1 удалённый + 1 новый = 8; узел 3 заменён, а не добавлен.
    assert_eq!(els.len(), 8);
    let find = |n: u128| els.get(&object_id_for(PrefabInstanceId::from_u128(1), &[nid(n)]));
    assert_eq!(find(1).unwrap().material, pid("t:wood"));
    assert!(find(2).is_none());
    assert!(matches!(
        find(3).unwrap().kind,
        StructureKind::Opening(OpeningKind::Window)
    ));
    assert!(matches!(
        find(100).unwrap().kind,
        StructureKind::Opening(OpeningKind::Door)
    ));
    // Родитель не изменился.
    let base = materialize(&reg, &instance("t:room", 1)).unwrap();
    assert_eq!(base.len(), 8);
    assert!(base.values().all(|e| matches!(e.kind, StructureKind::Wall)));
}

#[test]
fn cycles_missing_refs_versions_and_depth_are_detected() {
    // Цикл вложенности.
    let mut reg = PrefabRegistry::new();
    let mut a = PrefabDef::new(pid("t:a"));
    a.nodes
        .push(child_node(1, "t:b", GridTransform::IDENTITY, 0, None));
    let mut b = PrefabDef::new(pid("t:b"));
    b.nodes
        .push(child_node(1, "t:a", GridTransform::IDENTITY, 0, None));
    reg.insert(a).unwrap();
    reg.insert(b).unwrap();
    assert!(matches!(reg.validate(), Err(PrefabError::Cycle(_))));
    assert!(matches!(
        materialize(&reg, &instance("t:a", 1)),
        Err(PrefabError::Cycle(_))
    ));

    // Цикл наследования.
    let mut reg = PrefabRegistry::new();
    let mut x = PrefabDef::new(pid("t:x"));
    x.parent = Some(pid("t:y"));
    let mut y = PrefabDef::new(pid("t:y"));
    y.parent = Some(pid("t:x"));
    reg.insert(x).unwrap();
    reg.insert(y).unwrap();
    assert!(matches!(reg.validate(), Err(PrefabError::Cycle(_))));
    assert!(matches!(
        materialize(&reg, &instance("t:x", 1)),
        Err(PrefabError::Cycle(_))
    ));

    // Самоссылка.
    let mut reg = PrefabRegistry::new();
    let mut s = PrefabDef::new(pid("t:self"));
    s.nodes
        .push(child_node(1, "t:self", GridTransform::IDENTITY, 0, None));
    reg.insert(s).unwrap();
    assert!(matches!(reg.validate(), Err(PrefabError::Cycle(_))));

    // Отсутствующая ссылка и несовпадение версии.
    let mut reg = reg_with_room();
    let mut m = PrefabDef::new(pid("t:m"));
    m.nodes
        .push(child_node(1, "t:nope", GridTransform::IDENTITY, 0, None));
    reg.insert(m).unwrap();
    assert!(matches!(reg.validate(), Err(PrefabError::Missing(_))));
    let mut reg = reg_with_room();
    let mut v = PrefabDef::new(pid("t:v"));
    v.nodes
        .push(child_node(1, "t:room", GridTransform::IDENTITY, 0, Some(5)));
    reg.insert(v).unwrap();
    assert!(matches!(
        reg.validate(),
        Err(PrefabError::VersionMismatch {
            required: 5,
            actual: 1,
            ..
        })
    ));

    // Слишком глубокая цепочка.
    let mut reg = PrefabRegistry::new();
    for i in 0..(MAX_PREFAB_DEPTH + 3) {
        let mut d = PrefabDef::new(pid(&format!("t:d{i}")));
        if i + 1 < MAX_PREFAB_DEPTH + 3 {
            d.nodes.push(child_node(
                1,
                &format!("t:d{}", i + 1),
                GridTransform::IDENTITY,
                0,
                None,
            ));
        }
        reg.insert(d).unwrap();
    }
    assert!(matches!(reg.validate(), Err(PrefabError::TooDeep(_))));
}

#[test]
fn registry_rejects_duplicates_and_bad_data() {
    let mut reg = reg_with_room();
    assert!(matches!(
        reg.insert(room_def()),
        Err(PrefabError::DuplicatePrefab(_))
    ));
    let mut dup = PrefabDef::new(pid("t:dup"));
    dup.nodes.push(wall_node(1, GridEdge::along_x(0, 0), "t:m"));
    dup.nodes.push(wall_node(1, GridEdge::along_x(1, 0), "t:m"));
    assert!(matches!(
        reg.insert(dup),
        Err(PrefabError::DuplicateNode { .. })
    ));
    let mut bad = PrefabDef::new(pid("t:bad"));
    let mut n = wall_node(1, GridEdge::along_x(0, 0), "t:m");
    if let NodeContent::Element(t) = &mut n.content {
        t.thickness = f32::NAN;
    }
    bad.nodes.push(n);
    assert!(matches!(
        reg.insert(bad),
        Err(PrefabError::InvalidDimension(_))
    ));
}

#[test]
fn two_nodes_in_one_slot_are_a_collision() {
    let mut reg = PrefabRegistry::new();
    let mut d = PrefabDef::new(pid("t:c"));
    d.nodes.push(wall_node(1, GridEdge::along_x(0, 0), "t:m"));
    d.nodes.push(wall_node(2, GridEdge::along_x(0, 0), "t:m"));
    reg.insert(d).unwrap();
    assert!(matches!(
        materialize(&reg, &instance("t:c", 1)),
        Err(PrefabError::SlotCollision(_))
    ));
}

#[test]
fn first_spawn_creates_all_objects_and_is_idempotent() {
    let reg = reg_with_room();
    let inst = instance("t:room", 1);
    let mut w = world_with_floors(1);
    let r = reconcile(&reg, &inst, &BTreeSet::new(), &w).unwrap();
    assert_eq!(
        (r.commands.len(), r.spawned.len(), r.conflicts.len()),
        (8, 8, 0)
    );
    apply_all(&mut w, &r.commands);
    let again = reconcile(&reg, &inst, &r.spawned, &w).unwrap();
    assert!(
        again.commands.is_empty(),
        "повторное обновление без изменений ничего не делает"
    );
    assert_eq!(again.spawned, r.spawned);
}

#[test]
fn updating_a_prefab_updates_instances_but_keeps_instance_overrides() {
    let mut reg = reg_with_room();
    let mut inst = instance("t:room", 1);
    // Явное переопределение экземпляра: стена 1 — деревянная.
    inst.overrides.push(PathOverride {
        path: vec![nid(1)],
        value: NodeOverride {
            material: Some(pid("t:wood")),
            ..Default::default()
        },
    });
    let mut w = world_with_floors(1);
    let first = reconcile(&reg, &inst, &BTreeSet::new(), &w).unwrap();
    apply_all(&mut w, &first.commands);
    let wall1 = object_id_for(inst.id, &[nid(1)]);
    let wall2 = object_id_for(inst.id, &[nid(2)]);
    assert_eq!(w.element(wall1).unwrap().material, pid("t:wood"));
    assert_eq!(w.element(wall2).unwrap().material, pid("t:stone"));

    // Источник меняется: все стены становятся кирпичными, узел 8 удалён, добавлен новый.
    let mut v2 = room_def();
    v2.version = 2;
    for n in &mut v2.nodes {
        if let NodeContent::Element(t) = &mut n.content {
            t.material = pid("t:brick");
        }
    }
    v2.nodes.retain(|n| n.id != nid(8));
    v2.nodes
        .push(wall_node(50, GridEdge::along_x(5, 5), "t:brick"));
    reg.replace(v2).unwrap();

    let upd = reconcile(&reg, &inst, &first.spawned, &w).unwrap();
    assert!(upd.conflicts.is_empty());
    apply_all(&mut w, &upd.commands);
    assert_eq!(
        w.element(wall1).unwrap().material,
        pid("t:wood"),
        "переопределение экземпляра сохранилось"
    );
    assert_eq!(
        w.element(wall2).unwrap().material,
        pid("t:brick"),
        "остальное обновилось"
    );
    assert!(
        w.element(object_id_for(inst.id, &[nid(8)])).is_none(),
        "удалённый узел исчез"
    );
    assert!(
        w.element(object_id_for(inst.id, &[nid(50)])).is_some(),
        "новый узел появился"
    );
    assert_eq!(upd.spawned.len(), 8);
    // ObjectId сохранившихся объектов не изменились.
    assert!(upd.spawned.contains(&wall1) && upd.spawned.contains(&wall2));
}

#[test]
fn foreign_objects_are_never_destroyed_conflicts_are_reported() {
    let reg = reg_with_room();
    let inst = instance("t:room", 1);
    let mut w = world_with_floors(1);
    // Чужая стена уже занимает место узла 1.
    let squatter = ObjectId::from_u128(0xBAD);
    w.apply(WorldCommand::PlaceElement(
        astrocore_world::StructureElement {
            id: squatter,
            kind: StructureKind::Wall,
            placement: Placement::Edge(GridEdge::along_x(0, 0)),
            floor: FloorId(0),
            material: pid("t:other"),
            thickness: 0.2,
        },
    ))
    .unwrap();
    let r = reconcile(&reg, &inst, &BTreeSet::new(), &w).unwrap();
    assert_eq!(r.conflicts.len(), 1);
    assert_eq!(
        r.conflicts[0].kind,
        ConflictKind::SlotOccupied { by: squatter }
    );
    assert_eq!(r.spawned.len(), 7);
    apply_all(&mut w, &r.commands);
    assert!(w.element(squatter).is_some());
}

#[test]
fn missing_floor_is_a_conflict_not_a_failure() {
    let mut reg = reg_with_room();
    let mut tower = PrefabDef::new(pid("t:tower"));
    tower
        .nodes
        .push(child_node(1, "t:room", GridTransform::IDENTITY, 0, None));
    tower
        .nodes
        .push(child_node(2, "t:room", GridTransform::IDENTITY, 3, None));
    reg.insert(tower).unwrap();
    let w = world_with_floors(1);
    let r = reconcile(&reg, &instance("t:tower", 1), &BTreeSet::new(), &w).unwrap();
    assert_eq!(r.spawned.len(), 8);
    assert_eq!(r.conflicts.len(), 8);
    assert!(
        r.conflicts
            .iter()
            .all(|c| c.kind == ConflictKind::FloorMissing(FloorId(3)))
    );
}

#[test]
fn stair_target_floor_follows_floor_offsets() {
    let mut reg = PrefabRegistry::new();
    let mut d = PrefabDef::new(pid("t:stairs"));
    d.nodes.push(PrefabNode {
        id: nid(1),
        name: "stair".into(),
        content: NodeContent::Element(ElementTemplate {
            kind: LocalKind::Stair { to: 1 },
            placement: LocalPlacement::Run {
                start: GridCell::new(0, 0),
                dir: Facing::East,
                length: 3,
            },
            floor_offset: 0,
            material: pid("t:m"),
            thickness: 0.2,
        }),
    });
    reg.insert(d).unwrap();
    let mut inst = instance("t:stairs", 1);
    inst.base_floor = 1;
    inst.transform = GridTransform::rotate(1);
    let els = materialize(&reg, &inst).unwrap();
    let e = els.values().next().unwrap();
    assert_eq!(e.floor, FloorId(1));
    assert_eq!(e.kind, StructureKind::Stair { to: FloorId(2) });
    let Placement::Run { dir, length, .. } = e.placement else {
        panic!()
    };
    assert_eq!(
        (dir, length),
        (Facing::South, 3),
        "поворот меняет направление марша"
    );
    let mut w = world_with_floors(3);
    let r = reconcile(&reg, &inst, &BTreeSet::new(), &w).unwrap();
    apply_all(&mut w, &r.commands);
    assert_eq!(w.vertical_links().len(), 1);
}

#[test]
fn detach_gives_independent_ids_and_same_content() {
    let reg = reg_with_room();
    let inst = instance("t:room", 1);
    let mut w = world_with_floors(1);
    let r = reconcile(&reg, &inst, &BTreeSet::new(), &w).unwrap();
    apply_all(&mut w, &r.commands);
    let mut next = 5000u128;
    let (cmds, mapping) = detach(&w, &r.spawned, || {
        next += 1;
        ObjectId::from_u128(next)
    });
    assert_eq!(mapping.len(), 8);
    apply_all(&mut w, &cmds);
    assert_eq!(w.element_count(), 8);
    for (old, new) in &mapping {
        assert!(w.element(*old).is_none());
        assert!(w.element(*new).is_some());
    }
    // Обновление префаба старого экземпляра больше не затрагивает отделённые объекты.
    let again = reconcile(&reg, &inst, &r.spawned, &w).unwrap();
    assert_eq!(
        again
            .commands
            .iter()
            .filter(|c| matches!(c, WorldCommand::RemoveElement { .. }))
            .count(),
        0
    );
}

#[test]
fn diff_reports_node_changes() {
    let old = room_def();
    let mut new = room_def();
    new.nodes.retain(|n| n.id != nid(1));
    if let NodeContent::Element(t) = &mut new.nodes[0].content {
        t.material = pid("t:x");
    }
    new.nodes
        .push(wall_node(99, GridEdge::along_x(9, 9), "t:m"));
    let changes = diff_defs(&old, &new);
    assert!(changes.contains(&NodeChange::Removed(nid(1))));
    assert!(changes.contains(&NodeChange::Added(nid(99))));
    assert!(changes.iter().any(|c| matches!(c, NodeChange::Changed(_))));
}

#[test]
fn library_file_roundtrip_and_hostile_input() {
    let mut reg = reg_with_room();
    let mut tower = PrefabDef::new(pid("t:tower"));
    tower.nodes.push(child_node(
        1,
        "t:room",
        GridTransform::rotate(1),
        1,
        Some(1),
    ));
    reg.insert(tower).unwrap();
    let json = serde_json::to_string(&reg.to_file()).unwrap();
    let loaded = PrefabRegistry::from_json(&json).unwrap();
    assert_eq!(
        materialize(&loaded, &instance("t:tower", 1)).unwrap(),
        materialize(&reg, &instance("t:tower", 1)).unwrap()
    );
    assert!(matches!(
        PrefabRegistry::from_json("{ nope"),
        Err(PrefabError::File(_))
    ));
    let wrong_version = json.replacen("\"schema_version\":1", "\"schema_version\":9", 1);
    assert!(matches!(
        PrefabRegistry::from_json(&wrong_version),
        Err(PrefabError::File(_))
    ));
    // Библиотека с циклом отклоняется при загрузке.
    let mut cyc = PrefabRegistry::new();
    let mut a = PrefabDef::new(pid("t:a"));
    a.nodes
        .push(child_node(1, "t:a", GridTransform::IDENTITY, 0, None));
    cyc.insert(a).unwrap();
    assert!(PrefabRegistry::from_json(&serde_json::to_string(&cyc.to_file()).unwrap()).is_err());
}
