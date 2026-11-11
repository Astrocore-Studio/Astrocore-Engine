use astrocore_core::PrototypeId;
use astrocore_math::{GridCell, GridEdge};
use astrocore_physics::{LAYER_DYNAMIC, LAYER_STATIC};
use astrocore_world::{
    FloorDef, FloorId, Placement, StructureElement, StructureKind, World, WorldCommand, WorldConfig,
};

use super::*;

fn arena() -> World {
    let mut w = World::new("a", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 4.0,
    }))
    .unwrap();
    let m = PrototypeId::new("t:m").unwrap();
    let mut id = 1u128;
    let mut put = |w: &mut World, kind, placement| {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement,
            floor: FloorId(0),
            material: m.clone(),
            thickness: 0.2,
        }))
        .unwrap();
        id += 1;
    };
    for x in 0..12 {
        for z in 0..6 {
            put(
                &mut w,
                StructureKind::Floor,
                Placement::Cell(GridCell::new(x, z)),
            );
        }
    }
    for z in 0..6 {
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(6, z)),
        );
    }
    w
}

fn pair() -> (BoxWorld, AvianWorld) {
    let bw = BoxWorld::from_world(&arena());
    let av = AvianWorld::from_box_world(&bw);
    (bw, av)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.02
}

#[test]
fn statics_are_mirrored_from_the_box_world() {
    let (bw, av) = pair();
    assert_eq!(
        av.body_count(),
        bw.static_box_count() + bw.dynamic_box_count()
    );
    assert!(av.body_count() > 70);
}

#[test]
fn raycasts_agree_with_the_native_layer() {
    let (bw, av) = pair();
    for (o, d) in [
        (Vec3::new(1.0, 1.0, 3.0), Vec3::X),
        (Vec3::new(2.0, 1.5, 1.0), Vec3::new(1.0, 0.0, 0.3)),
        (Vec3::new(3.0, 5.0, 3.0), Vec3::NEG_Y),
        (Vec3::new(10.0, 1.0, 3.0), Vec3::NEG_X),
    ] {
        let a = PhysicsQuery::raycast_masked(&av, o, d, 50.0, LAYER_ALL);
        let b = PhysicsQuery::raycast_masked(&bw, o, d, 50.0, LAYER_ALL);
        let (a, b) = (a.expect("avian"), b.expect("native"));
        assert!(
            near(a.distance, b.distance),
            "{o} {d}: {} vs {}",
            a.distance,
            b.distance
        );
        assert!(
            a.normal.dot(b.normal) > 0.99,
            "{:?} vs {:?}",
            a.normal,
            b.normal
        );
    }
    assert!(
        PhysicsQuery::raycast_masked(&av, Vec3::new(1.0, 1.0, 3.0), Vec3::NEG_X, 50.0, LAYER_ALL)
            .is_none()
    );
}

#[test]
fn overlaps_shapecasts_and_layers_agree() {
    let (bw, av) = pair();
    let area = Aabb::new(Vec3::new(5.0, 0.1, 1.0), Vec3::new(7.0, 2.0, 2.0));
    let (a, b) = (
        PhysicsQuery::overlap_masked(&av, &area, LAYER_ALL),
        PhysicsQuery::overlap_masked(&bw, &area, LAYER_ALL),
    );
    assert_eq!(a, b);
    assert!(!a.is_empty());
    assert!(
        PhysicsQuery::overlap_masked(&av, &area, LAYER_DYNAMIC).is_empty(),
        "динамического нет"
    );
    assert!(!PhysicsQuery::overlap_masked(&av, &area, LAYER_STATIC).is_empty());

    let me = Aabb::new(Vec3::new(1.0, 0.3, 2.6), Vec3::new(1.8, 2.0, 3.4));
    let motion = Vec3::new(10.0, 0.0, 0.0);
    let (a, b) = (
        PhysicsQuery::shapecast(&av, &me, motion, LAYER_ALL).expect("avian"),
        PhysicsQuery::shapecast(&bw, &me, motion, LAYER_ALL).expect("native"),
    );
    assert!(
        near(a.fraction, b.fraction),
        "{} vs {}",
        a.fraction,
        b.fraction
    );
    // Стена из соседних секций: при равной дальности первой может оказаться любая из них.
    assert!(a.id.as_uuid().as_u128() >= 0x49 && b.id.as_uuid().as_u128() >= 0x49);
    assert!(PhysicsQuery::shapecast(&av, &me, Vec3::new(3.0, 0.0, 0.0), LAYER_ALL).is_none());

    let sunk = Aabb::new(Vec3::new(1.0, -0.05, 1.0), Vec3::new(1.6, 1.7, 1.6));
    let (ca, cb) = (
        PhysicsQuery::contacts(&av, &sunk, LAYER_ALL),
        PhysicsQuery::contacts(&bw, &sunk, LAYER_ALL),
    );
    assert_eq!(ca.len(), cb.len());
    assert_eq!(ca[0].id, cb[0].id);
    assert_eq!(ca[0].normal, cb[0].normal);
    assert!(near(ca[0].depth, cb[0].depth));
}

#[test]
fn dynamic_bodies_fall_land_and_stop_at_walls() {
    let (_, mut av) = pair();
    let ball = ObjectId::from_u128(9000);
    av.spawn_dynamic(
        ball,
        Aabb::new(Vec3::new(2.0, 3.0, 2.0), Vec3::new(2.5, 3.5, 2.5)),
        LAYER_DYNAMIC,
    );
    let start = av.position(ball).unwrap();
    av.step(120);
    let landed = av.position(ball).unwrap();
    assert!(landed.y < start.y - 2.0, "упал: {start} → {landed}");
    assert!(
        landed.y > 0.2,
        "стоит на полу, а не проваливается: {landed}"
    );
    assert!(
        av.linear_velocity(ball).unwrap().length() < 0.5,
        "успокоился"
    );
    // Толчок в сторону стены (x = 6): тело останавливается перед ней.
    av.set_linear_velocity(ball, Vec3::new(8.0, 0.0, 0.0));
    av.step(180);
    let p = av.position(ball).unwrap();
    assert!(p.x < 6.0 && p.x > 3.0, "уткнулось в стену: {p}");
    // Динамическое тело видно запросам по своему слою и не видно по чужому.
    let at = Aabb::new(p - Vec3::splat(0.4), p + Vec3::splat(0.4));
    assert!(PhysicsQuery::overlap_masked(&av, &at, LAYER_DYNAMIC).contains(&ball));
    assert!(!PhysicsQuery::overlap_masked(&av, &at, LAYER_STATIC).contains(&ball));
}

#[test]
fn kinematic_bodies_move_and_sync_follows_world_changes() {
    let mut w = arena();
    let bw = BoxWorld::from_world(&w);
    let mut av = AvianWorld::from_box_world(&bw);
    let lift = ObjectId::from_u128(9001);
    av.spawn_kinematic(
        lift,
        Aabb::new(Vec3::new(1.0, 0.0, 1.0), Vec3::new(2.0, 0.2, 2.0)),
        LAYER_DYNAMIC,
    );
    av.set_linear_velocity(lift, Vec3::new(0.0, 1.0, 0.0));
    av.step(60);
    let y = av.position(lift).unwrap().y;
    assert!((y - 1.1).abs() < 0.2, "поднялась примерно на 1 м: {y}");
    // Мир меняется (стену убрали): синхронизация убирает коробки, динамика остаётся.
    let wall = w
        .elements()
        .find(|e| e.kind == StructureKind::Wall)
        .unwrap()
        .id;
    w.apply(WorldCommand::RemoveElement { id: wall }).unwrap();
    let regions = w.take_dirty();
    let mut bw2 = bw.clone();
    bw2.rebuild_regions(&w, &regions);
    let before = av.body_count();
    av.sync_statics(&bw2);
    assert_eq!(av.body_count(), before - 1);
    assert!(
        av.position(lift).is_some(),
        "кинематическое тело не удалено"
    );
}
