use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::glam::Vec3;
use astrocore_math::{Aabb, Facing, GridCell, GridEdge};
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, StructureElement, StructureKind, World,
    WorldCommand, WorldConfig,
};

use super::*;

fn world() -> World {
    let mut w = World::new("t", WorldConfig::default()).unwrap();
    for f in 0..2 {
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

fn put(w: &mut World, id: u128, kind: StructureKind, placement: Placement, floor: i32) {
    w.apply(WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(id),
        kind,
        placement,
        floor: FloorId(floor),
        material: PrototypeId::new("t:m").unwrap(),
        thickness: 0.2,
    }))
    .unwrap();
}

fn floor_slab(w: &mut World, id: u128, x: i32, z: i32) {
    put(
        w,
        id,
        StructureKind::Floor,
        Placement::Cell(GridCell::new(x, z)),
        0,
    );
}

/// Пол 12×6 и стена поперёк на x = 6 (ребра z = 0..6).
fn arena() -> World {
    let mut w = world();
    let mut id = 1;
    for x in 0..12 {
        for z in 0..6 {
            floor_slab(&mut w, id, x, z);
            id += 1;
        }
    }
    id = 1000;
    for z in 0..6 {
        put(
            &mut w,
            id,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(6, z)),
            0,
        );
        id += 1;
    }
    w
}

fn walk(
    bw: &BoxWorld,
    state: &mut CharacterState,
    shape: &CharacterShape,
    dir: Vec3,
    seconds: f32,
) {
    let dt = 1.0 / 30.0;
    for _ in 0..(seconds / dt) as usize {
        move_character(bw, state, shape, dir * 4.0 * dt, dt);
    }
}

/// Поверхность пола ≈ толщина плиты 0.2 м.
const FLOOR_TOP: f32 = 0.2;

#[test]
fn character_lands_on_floor_and_stands() {
    let bw = BoxWorld::from_world(&arena());
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(2.0, 3.0, 2.0));
    walk(&bw, &mut s, &shape, Vec3::ZERO, 2.0);
    assert!(s.grounded);
    assert!((s.pos.y - FLOOR_TOP).abs() < 0.01, "{:?}", s.pos);
}

#[test]
fn walls_block_movement_and_character_slides_along_them() {
    let bw = BoxWorld::from_world(&arena());
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(2.0, 0.3, 2.0));
    walk(&bw, &mut s, &shape, Vec3::X, 3.0);
    assert!(
        s.pos.x < 6.0 - 0.1 - shape.radius + 0.05,
        "упёрлись в стену: x={}",
        s.pos.x
    );
    assert!(s.pos.x > 5.4, "дошли до стены вплотную: x={}", s.pos.x);
    // Движение по диагонали вдоль стены: Z продолжает меняться.
    let z0 = s.pos.z;
    walk(
        &bw,
        &mut s,
        &shape,
        Vec3::new(1.0, 0.0, 1.0).normalize(),
        0.5,
    );
    assert!(s.pos.z > z0 + 0.5, "скользим вдоль стены");
    assert!(s.pos.x < 6.0 - 0.1 - shape.radius + 0.05);
}

#[test]
fn closed_door_blocks_and_open_door_lets_through() {
    let mut w = arena();
    // Дверь вместо стены на ребре (6, 2).
    let wall = w
        .edge_element(FloorId(0), GridEdge::along_z(6, 2))
        .unwrap()
        .id;
    w.apply(WorldCommand::RemoveElement { id: wall }).unwrap();
    put(
        &mut w,
        900,
        StructureKind::Opening(OpeningKind::Door),
        Placement::Edge(GridEdge::along_z(6, 2)),
        0,
    );
    let mut bw = BoxWorld::from_world(&w);
    let shape = CharacterShape::default();
    let door = ObjectId::from_u128(900);

    bw.sync_doors(&w, |_| false);
    assert_eq!(bw.dynamic_box_count(), 1);
    let mut s = CharacterState::at(Vec3::new(3.0, 0.3, 2.5));
    walk(&bw, &mut s, &shape, Vec3::X, 3.0);
    assert!(s.pos.x < 6.0, "закрытая дверь не пускает: x={}", s.pos.x);

    bw.sync_doors(&w, |id| id == door);
    assert_eq!(bw.dynamic_box_count(), 0);
    walk(&bw, &mut s, &shape, Vec3::X, 2.0);
    assert!(s.pos.x > 6.5, "открытая дверь пропускает: x={}", s.pos.x);
}

#[test]
fn character_climbs_a_staircase_to_the_upper_floor() {
    let mut w = world();
    // Пол первого этажа под маршем и площадка второго этажа за ним.
    for x in 0..12 {
        put(
            &mut w,
            1 + x as u128,
            StructureKind::Floor,
            Placement::Cell(GridCell::new(x, 0)),
            0,
        );
    }
    put(
        &mut w,
        500,
        StructureKind::Stair { to: FloorId(1) },
        Placement::Run {
            start: GridCell::new(2, 0),
            dir: Facing::East,
            length: 5,
        },
        0,
    );
    // Площадка верхнего этажа: перекрытие этажа 1 начинается в конце марша.
    for x in 7..12 {
        put(
            &mut w,
            600 + x as u128,
            StructureKind::Floor,
            Placement::Cell(GridCell::new(x, 0)),
            1,
        );
    }
    let bw = BoxWorld::from_world(&w);
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(0.7, 0.4, 0.5));
    walk(&bw, &mut s, &shape, Vec3::X, 2.5);
    // Верхняя площадка: поверхность плиты 4.0 + 0.2.
    assert!(s.pos.x > 8.0, "дошли до площадки: x={}", s.pos.x);
    assert!(
        (s.pos.y - 4.2).abs() < 0.05,
        "стоим на втором этаже: y={}",
        s.pos.y
    );
    assert!(s.grounded);
}

#[test]
fn character_falls_off_edges_and_stops_under_ceilings() {
    let mut w = world();
    floor_slab(&mut w, 1, 0, 0);
    let bw = BoxWorld::from_world(&w);
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(0.5, 0.3, 0.5));
    walk(&bw, &mut s, &shape, Vec3::X, 2.0);
    assert!(
        s.pos.y < -1.0,
        "за краем плиты персонаж падает: y={}",
        s.pos.y
    );
    assert!(!s.grounded);

    // Низкий потолок над головой не пропускает, но и не телепортирует.
    let mut w = world();
    floor_slab(&mut w, 1, 0, 0);
    put(
        &mut w,
        2,
        StructureKind::Ceiling,
        Placement::Cell(GridCell::new(0, 0)),
        0,
    );
    let bw = BoxWorld::from_world(&w);
    let mut s = CharacterState::at(Vec3::new(0.5, 0.3, 0.5));
    walk(&bw, &mut s, &shape, Vec3::ZERO, 1.0);
    assert!(s.grounded && (s.pos.y - FLOOR_TOP).abs() < 0.01);
}

#[test]
fn movement_is_deterministic() {
    let bw = BoxWorld::from_world(&arena());
    let shape = CharacterShape::default();
    let run = || {
        let mut s = CharacterState::at(Vec3::new(1.0, 2.0, 1.0));
        for i in 0..200 {
            let dir = if i % 40 < 20 {
                Vec3::X
            } else {
                Vec3::new(0.0, 0.0, 1.0)
            };
            move_character(&bw, &mut s, &shape, dir * 0.1, 1.0 / 30.0);
        }
        s
    };
    assert_eq!(run(), run());
}

#[test]
fn raycast_finds_nearest_surface_with_normal() {
    let bw = BoxWorld::from_world(&arena());
    let hit = bw
        .raycast(Vec3::new(2.0, 1.0, 2.5), Vec3::X, 20.0)
        .expect("стена на пути");
    assert!((hit.distance - (6.0 - 0.1 - 2.0)).abs() < 1e-4, "{hit:?}");
    assert_eq!(hit.normal, Vec3::NEG_X);
    assert!(
        bw.raycast(Vec3::new(2.0, 1.0, 2.5), Vec3::X, 1.0).is_none(),
        "не дотягивается"
    );
    assert!(
        bw.raycast(Vec3::new(2.0, 1.0, 2.5), Vec3::ZERO, 10.0)
            .is_none(),
        "нулевое направление"
    );
    let down = bw
        .raycast(Vec3::new(2.0, 3.0, 2.0), Vec3::NEG_Y, 10.0)
        .unwrap();
    assert!((down.point.y - FLOOR_TOP).abs() < 1e-4);
    assert_eq!(down.normal, Vec3::Y);
}

#[test]
fn raycast_prefers_closer_hit_and_includes_dynamic_boxes() {
    let mut bw = BoxWorld::from_world(&arena());
    let dynamic = ObjectId::from_u128(77);
    bw.set_dynamic(
        dynamic,
        Some(Aabb::new(
            Vec3::new(4.0, 0.0, 2.0),
            Vec3::new(4.2, 3.0, 3.0),
        )),
    );
    let hit = bw.raycast(Vec3::new(2.0, 1.0, 2.5), Vec3::X, 20.0).unwrap();
    assert_eq!(hit.id, dynamic);
    bw.set_dynamic(dynamic, None);
    assert_ne!(
        bw.raycast(Vec3::new(2.0, 1.0, 2.5), Vec3::X, 20.0)
            .unwrap()
            .id,
        dynamic
    );
}

#[test]
fn overlap_query_is_sorted_and_unique() {
    let bw = BoxWorld::from_world(&arena());
    let ids = bw.overlap_aabb(&Aabb::new(
        Vec3::new(5.0, 0.5, 0.0),
        Vec3::new(7.0, 3.0, 6.0),
    ));
    assert_eq!(ids.len(), 6, "шесть стенных сегментов");
    assert!(ids.windows(2).all(|p| p[0] < p[1]));
    assert!(
        bw.overlap_aabb(&Aabb::new(Vec3::splat(100.0), Vec3::splat(101.0)))
            .is_empty()
    );
}

#[test]
fn collision_follows_dirty_regions() {
    let mut w = arena();
    w.take_dirty();
    let mut bw = BoxWorld::from_world(&w);
    let probe = Aabb::new(Vec3::new(5.8, 0.5, 2.0), Vec3::new(6.2, 1.5, 3.0));
    assert!(!bw.overlap_aabb(&probe).is_empty());
    let wall = w
        .edge_element(FloorId(0), GridEdge::along_z(6, 2))
        .unwrap()
        .id;
    w.apply(WorldCommand::RemoveElement { id: wall }).unwrap();
    let dirty = w.take_dirty();
    bw.rebuild_regions(&w, &dirty);
    assert!(
        bw.overlap_aabb(&probe).is_empty(),
        "стена исчезла из коллизии после пересборки"
    );
    assert_eq!(bw.static_box_count(), 12 * 6 + 5);
}

#[test]
fn walls_on_chunk_borders_collide_from_both_sides() {
    let mut w = world();
    for x in 28..36 {
        put(
            &mut w,
            1 + x as u128,
            StructureKind::Floor,
            Placement::Cell(GridCell::new(x, 0)),
            0,
        );
    }
    put(
        &mut w,
        900,
        StructureKind::Wall,
        Placement::Edge(GridEdge::along_z(32, 0)),
        0,
    );
    let bw = BoxWorld::from_world(&w);
    let shape = CharacterShape::default();
    let mut east = CharacterState::at(Vec3::new(30.0, 0.3, 0.5));
    walk(&bw, &mut east, &shape, Vec3::X, 2.0);
    assert!(east.pos.x < 32.0, "x={}", east.pos.x);
    let mut west = CharacterState::at(Vec3::new(34.0, 0.3, 0.5));
    walk(&bw, &mut west, &shape, Vec3::NEG_X, 2.0);
    assert!(west.pos.x > 32.0, "x={}", west.pos.x);
}

#[test]
fn triggers_report_enter_and_exit_once() {
    let mut t = TriggerSet::default();
    let (zone, actor) = (ObjectId::from_u128(1), ObjectId::from_u128(2));
    t.insert(zone, Aabb::new(Vec3::ZERO, Vec3::splat(2.0)));
    let inside = Aabb::new(Vec3::splat(0.5), Vec3::splat(1.0));
    let outside = Aabb::new(Vec3::splat(5.0), Vec3::splat(6.0));
    assert_eq!(t.update(actor, &outside), vec![]);
    assert_eq!(
        t.update(actor, &inside),
        vec![TriggerEvent::Entered {
            trigger: zone,
            actor
        }]
    );
    assert_eq!(t.update(actor, &inside), vec![], "повторно не сообщает");
    assert_eq!(
        t.update(actor, &outside),
        vec![TriggerEvent::Exited {
            trigger: zone,
            actor
        }]
    );
    t.update(actor, &inside);
    assert_eq!(
        t.forget(actor),
        vec![TriggerEvent::Exited {
            trigger: zone,
            actor
        }]
    );
    assert!(t.forget(actor).is_empty());
}

#[test]
fn ray_aabb_edge_cases() {
    let b = Aabb::new(Vec3::ZERO, Vec3::ONE);
    // Старт внутри коробки: расстояние 0.
    assert_eq!(
        ray_aabb(Vec3::splat(0.5), Vec3::X, &b).map(|h| h.0),
        Some(0.0)
    );
    // Параллельно грани снаружи — промах; параллельно внутри плиты — попадание.
    assert!(ray_aabb(Vec3::new(-1.0, 2.0, 0.5), Vec3::X, &b).is_none());
    assert!(ray_aabb(Vec3::new(-1.0, 0.5, 0.5), Vec3::X, &b).is_some());
    // Луч от коробки — промах.
    assert!(ray_aabb(Vec3::new(2.0, 0.5, 0.5), Vec3::X, &b).is_none());
}

#[test]
fn freeform_objects_block_characters_with_their_bounding_box() {
    use astrocore_world::FreePlacement;
    let mut w = arena();
    w.apply(WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(4242),
        kind: StructureKind::Freeform,
        placement: Placement::Free(FreePlacement::from_position(
            [4.0, FLOOR_TOP, 2.5],
            1.0,
            0.0,
            [0.4, 1.0, 0.4],
        )),
        floor: FloorId(0),
        material: PrototypeId::new("t:statue").unwrap(),
        thickness: 0.1,
    }))
    .unwrap();
    let bw = BoxWorld::from_world(&w);
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(1.5, 0.3, 2.5));
    walk(&bw, &mut s, &shape, Vec3::X, 2.0);
    assert!(
        s.pos.x < 4.0 - 0.4,
        "статуя остановила игрока: x={}",
        s.pos.x
    );
    assert_eq!(
        bw.raycast(Vec3::new(1.0, 1.0, 2.5), Vec3::X, 10.0)
            .unwrap()
            .id,
        ObjectId::from_u128(4242)
    );
}

mod queries {
    use super::*;
    use crate::{LAYER_DYNAMIC, LAYER_STATIC, carry_riders};

    fn bw_with(boxes: &[(u128, Aabb)], dynamics: &[(u128, Aabb)]) -> BoxWorld {
        let mut bw = BoxWorld {
            chunk_size_m: 32.0,
            ..BoxWorld::default()
        };
        let list: Vec<_> = boxes
            .iter()
            .map(|(i, b)| (ObjectId::from_u128(*i), *b))
            .collect();
        bw.statics
            .insert((astrocore_math::ChunkCoord::new(0, 0), FloorId(0)), list);
        for (i, b) in dynamics {
            bw.set_dynamic(ObjectId::from_u128(*i), Some(*b));
        }
        bw
    }

    fn cube(min: [f32; 3], max: [f32; 3]) -> Aabb {
        Aabb::new(Vec3::from(min), Vec3::from(max))
    }

    #[test]
    fn layers_filter_rays_and_overlaps() {
        let wall = cube([4.0, 0.0, 0.0], [5.0, 2.0, 2.0]);
        let door = cube([2.0, 0.0, 0.0], [3.0, 2.0, 2.0]);
        let bw = bw_with(&[(1, wall)], &[(2, door)]);
        let o = Vec3::new(0.0, 1.0, 1.0);
        let all = bw.raycast_masked(o, Vec3::X, 20.0, LAYER_ALL).unwrap();
        assert_eq!(all.id, ObjectId::from_u128(2));
        let only_static = bw.raycast_masked(o, Vec3::X, 20.0, LAYER_STATIC).unwrap();
        assert_eq!(only_static.id, ObjectId::from_u128(1));
        assert!(bw.raycast_masked(o, Vec3::X, 20.0, 1 << 5).is_none());
        let area = cube([0.0, 0.0, 0.0], [10.0, 2.0, 2.0]);
        assert_eq!(bw.overlap_masked(&area, LAYER_DYNAMIC).len(), 1);
        assert_eq!(bw.overlap_masked(&area, LAYER_ALL).len(), 2);
        // Переопределённый слой.
        let mut bw = bw;
        bw.set_layer(ObjectId::from_u128(2), LAYER_TRIGGER);
        assert!(bw.overlap_masked(&area, LAYER_DYNAMIC).is_empty());
        assert_eq!(bw.overlap_masked(&area, LAYER_TRIGGER).len(), 1);
    }

    #[test]
    fn shapecast_stops_at_the_first_obstacle() {
        let wall = cube([5.0, 0.0, -5.0], [6.0, 3.0, 5.0]);
        let bw = bw_with(&[(1, wall)], &[]);
        let me = cube([0.0, 0.0, -0.4], [0.8, 1.8, 0.4]);
        let hit = bw
            .shapecast(&me, Vec3::new(10.0, 0.0, 0.0), LAYER_ALL)
            .unwrap();
        // Коробка шириной 0.8 упирается передней гранью в x = 5: центр останавливается на 4.6.
        assert!((hit.fraction - 0.42).abs() < 1e-4, "{}", hit.fraction);
        assert_eq!(hit.normal, Vec3::NEG_X);
        assert!(
            bw.shapecast(&me, Vec3::new(3.0, 0.0, 0.0), LAYER_ALL)
                .is_none()
        );
        // Уже внутри преграды: касание в нуле.
        let inside = cube([4.8, 0.0, 0.0], [5.4, 1.0, 0.5]);
        assert_eq!(
            bw.shapecast(&inside, Vec3::X, LAYER_ALL).unwrap().fraction,
            0.0
        );
    }

    #[test]
    fn contacts_report_minimal_push_out() {
        let floor = cube([-10.0, -0.2, -10.0], [10.0, 0.0, 10.0]);
        let bw = bw_with(&[(1, floor)], &[]);
        let sunk = cube([0.0, -0.05, 0.0], [0.6, 1.75, 0.6]);
        let c = bw.contacts(&sunk, LAYER_ALL);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].normal, Vec3::Y);
        assert!((c[0].depth - 0.05).abs() < 1e-5);
        assert!(
            bw.contacts(&cube([0.0, 1.0, 0.0], [0.6, 2.8, 0.6]), LAYER_ALL)
                .is_empty()
        );
    }

    #[test]
    fn moving_platform_carries_riders_and_not_bystanders() {
        let plat = cube([0.0, 0.0, 0.0], [3.0, 0.2, 3.0]);
        let mut bw = bw_with(&[], &[(7, plat)]);
        let shape = CharacterShape::default();
        let mut riders = [
            CharacterState::at(Vec3::new(1.5, 0.2, 1.5)),
            CharacterState::at(Vec3::new(9.0, 0.2, 9.0)),
            CharacterState::at(Vec3::new(1.5, 1.2, 1.5)),
        ];
        let id = ObjectId::from_u128(7);
        let n = carry_riders(&mut bw, id, Vec3::new(0.0, 4.0, 0.0), &mut riders, &shape);
        assert_eq!(n, 1);
        assert!((riders[0].pos.y - 4.2).abs() < 1e-5);
        assert_eq!(riders[1].pos.y, 0.2);
        assert_eq!(riders[2].pos.y, 1.2);
        assert!((bw.dynamic_box(id).unwrap().max.y - 4.2).abs() < 1e-5);
        // Персонаж на поднятой платформе остаётся на ней, а не проваливается.
        assert!(
            bw.overlap_masked(&cube([0.0, 4.0, 0.0], [1.0, 4.1, 1.0]), LAYER_ALL)
                .len()
                == 1
        );
    }
}
