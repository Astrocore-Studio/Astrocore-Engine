//! Приёмочные проверки AC-VS1 (headless-часть).

use std::collections::HashSet;

use astrocore_core::ObjectId;
use astrocore_geometry::{MeshData, build_chunk_meshes, element_bounds};
use astrocore_math::glam::Vec3;
use astrocore_math::{ChunkCoord, GridCell, GridEdge};
use astrocore_navigation::{PathError, PathOptions, find_path};
use astrocore_persistence::Store;
use astrocore_protocol::{ServerMessage, decode, encode};
use astrocore_spatial::{Channel, Topology, TopologyChange, diff};
use astrocore_sprites::{Direction, DirectionSet, select_direction};
use astrocore_vertical_slice::*;
use astrocore_world::{
    DirtyFlags, FloorId, ReplicaOutcome, StructureKind, World, WorldConfig, WorldOp,
};

const OUTSIDE: GridCell = GridCell::new(MIN_X + 2, -2);
const INSIDE: GridCell = GridCell::new(MIN_X + 2, 2);

fn closed_door(s: &Slice) -> PathOptions {
    PathOptions {
        closed_doors: HashSet::from([s.door]),
        ..Default::default()
    }
}

#[test]
fn building_has_expected_structure_and_straddles_chunks() {
    let s = Slice::build().unwrap();
    // На этаж: 24 стены (одна заменена дверью) + 36 перекрытий; площадка у входа (24 клетки);
    // две лестницы; 8 клеток перекрытий над маршами убраны; одна свободная модель (статуя).
    assert_eq!(s.world.element_count(), 3 * (24 + 36) + 24 + 2 - 8 + 1);
    assert_eq!(s.world.element_count(), 199);
    // Ревизии: 3 этажа + все размещения + замена стены дверью + площадка + пробивки + лестницы + статуя.
    assert_eq!(s.world.revision().0, 220);
    // Здание пересекает границы чанков и по X (x = 32), и по Z (северная стена лежит на z = 0).
    assert_eq!(
        s.world.populated_chunks(FloorId(0)),
        vec![
            ChunkCoord::new(0, -1),
            ChunkCoord::new(0, 0),
            ChunkCoord::new(1, -1),
            ChunkCoord::new(1, 0)
        ]
    );
}

#[test]
fn meshes_cover_each_floor_in_its_own_height_range() {
    let s = Slice::build().unwrap();
    for f in 0..FLOORS {
        let (lo, hi) = (f as f32 * FLOOR_HEIGHT_M, (f + 1) as f32 * FLOOR_HEIGHT_M);
        let mut tris = 0;
        for chunk in s.world.populated_chunks(FloorId(f)) {
            for mesh in build_chunk_meshes(&s.world, chunk, FloorId(f)).values() {
                let b = mesh.bounds().unwrap();
                assert!(
                    b.min.y >= lo - 1e-5 && b.max.y <= hi + 1e-5,
                    "этаж {f}: {b:?}"
                );
                tris += mesh.triangle_count();
            }
        }
        // По 12 треугольников на коробку. Дверь геометрии не даёт; статуя — свободная модель,
        // её рисует клиент по идентификатору, в структурные меши она не входит.
        let stair_steps = astrocore_geometry::stair_step_count(FLOOR_HEIGHT_M);
        let boxes = match f {
            0 => 23 + 36 + 24 + stair_steps, // стены, пол, площадка, лестница на 2-й этаж
            1 => 24 + 32 + stair_steps,      // пол без проёма, лестница на 3-й этаж
            _ => 24 + 32,
        };
        assert_eq!(tris, boxes * 12, "этаж {f}");
    }
}

#[test]
fn ground_floor_has_one_room_with_a_door_portal() {
    let s = Slice::build().unwrap();
    let t = Topology::build(&s.world, FloorId(0)).unwrap();
    assert_eq!(t.interior_rooms().count(), 1);
    assert_eq!(t.portals.len(), 1);
    assert_eq!(t.portals[0].element, s.door);
    assert!(t.diagnostics().is_empty());
    // Закрытые верхние этажи без дверей — замкнутые комнаты без прохода: валидатор это показывает.
    let upper = Topology::build(&s.world, FloorId(1)).unwrap();
    assert_eq!(upper.diagnostics().len(), 1);
}

#[test]
fn path_requires_open_door_until_the_wall_is_destroyed() {
    let mut s = Slice::build().unwrap();
    let open = find_path(
        &s.world,
        FloorId(0),
        OUTSIDE,
        INSIDE,
        &PathOptions::default(),
    )
    .unwrap();
    assert!(
        open.contains(&GridCell::new(31, 0)),
        "путь идёт через дверной проём"
    );
    assert_eq!(
        find_path(&s.world, FloorId(0), OUTSIDE, INSIDE, &closed_door(&s)),
        Err(PathError::NoPath)
    );

    s.breach().unwrap();
    let via_breach = find_path(&s.world, FloorId(0), OUTSIDE, INSIDE, &closed_door(&s)).unwrap();
    assert_eq!(via_breach.last(), Some(&INSIDE));
}

#[test]
fn wall_destruction_marks_every_derived_view_dirty_and_updates_topology() {
    let mut s = Slice::build().unwrap();
    let before = Topology::build(&s.world, FloorId(0)).unwrap();
    s.world.take_dirty();

    s.breach().unwrap();
    let dirty = s.world.take_dirty();
    assert!(!dirty.is_empty());
    for d in &dirty {
        assert_eq!(d.floor, FloorId(0));
        for flag in [
            DirtyFlags::GEOMETRY,
            DirtyFlags::COLLISION,
            DirtyFlags::NAVIGATION,
            DirtyFlags::TOPOLOGY,
        ] {
            assert!(d.flags.contains(flag), "{d:?} без {flag:?}");
        }
    }
    // Пролом на x=35 затрагивает только чанк 1 (клетки 34 и 35), границу x=32 — нет.
    assert_eq!(
        dirty.iter().map(|d| d.chunk).collect::<Vec<_>>(),
        vec![ChunkCoord::new(1, 0)]
    );

    let after = Topology::build(&s.world, FloorId(0)).unwrap();
    let changes = diff(&before, &after);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, TopologyChange::RoomRemoved { .. })),
        "{changes:?}"
    );
    assert_eq!(
        after.interior_rooms().count(),
        0,
        "комната слилась с улицей"
    );
    // Воздух теперь свободно проходит наружу, даже при закрытой двери.
    let ext = after.exterior().unwrap().id;
    assert_eq!(after.reachable(ext, Channel::Air, |_| false).len(), 1);
}

#[test]
fn replicas_converge_despite_reordering_and_duplicates() {
    let mut s = Slice::build().unwrap();
    s.breach().unwrap();
    let mut a = World::new("vertical-slice", WorldConfig::default()).unwrap();
    let mut b = World::new("vertical-slice", WorldConfig::default()).unwrap();

    // Клиент A получает всё по порядку, но с дублями.
    for op in &s.log {
        assert_eq!(a.apply_replicated(op), ReplicaOutcome::Applied);
        assert_eq!(a.apply_replicated(op), ReplicaOutcome::Duplicate);
    }
    // Клиент B получает перемешанный поток; потерянные операции запрашивает повторно.
    let mut pending: Vec<&WorldOp> = s.log.iter().collect();
    pending.reverse();
    let mut rounds = 0;
    while b.revision().0 < s.world.revision().0 {
        rounds += 1;
        assert!(rounds < 10_000, "репликация зациклилась");
        let mut progressed = false;
        for op in pending.clone() {
            if let ReplicaOutcome::Applied = b.apply_replicated(op) {
                progressed = true;
            }
        }
        assert!(progressed, "нет прогресса: требуется snapshot");
    }
    assert_eq!(a.content_hash(), s.world.content_hash());
    assert_eq!(b.content_hash(), s.world.content_hash());
    assert!(
        a.edge_element(FloorId(0), BREACH_EDGE).is_none()
            && b.edge_element(FloorId(0), BREACH_EDGE).is_none()
    );
}

#[test]
fn operations_survive_the_wire_format() {
    let mut s = Slice::build().unwrap();
    s.breach().unwrap();
    let mut client = World::new("vertical-slice", WorldConfig::default()).unwrap();
    let mut bytes_total = 0;
    for op in &s.log {
        let packet = encode(&ServerMessage::WorldOp(op.clone())).unwrap();
        bytes_total += packet.len();
        let ServerMessage::WorldOp(received) = decode::<ServerMessage>(&packet).unwrap() else {
            panic!("ожидалась WorldOp");
        };
        assert_eq!(client.apply_replicated(&received), ReplicaOutcome::Applied);
    }
    assert_eq!(client.content_hash(), s.world.content_hash());
    assert!(
        bytes_total < 200 * s.log.len(),
        "операция должна быть компактной ({bytes_total} байт)"
    );
}

#[test]
fn save_restart_restores_the_same_world_including_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut s = Slice::build().unwrap();
    // Снимок строго до разрушения, затем разрушение попадает только в журнал.
    let mut pre = Slice::build().unwrap();
    store.save_snapshot(&pre.world).unwrap();
    let op = s.breach().unwrap();
    pre.exec(op.command.clone()).unwrap();
    store.append(&op).unwrap();

    let recovered = Store::open(dir.path()).unwrap().recover().unwrap().unwrap();
    assert_eq!(recovered.replayed, 1);
    assert_eq!(recovered.world.content_hash(), s.world.content_hash());
    assert!(
        recovered.world.element(s.door).is_some(),
        "ObjectId двери сохранился"
    );
    assert!(
        recovered
            .world
            .edge_element(FloorId(0), BREACH_EDGE)
            .is_none(),
        "разрушение сохранилось"
    );
}

#[test]
fn axes_agree_between_geometry_navigation_and_sprites() {
    let s = Slice::build().unwrap();
    let floor0 = s.world.floor(FloorId(0)).unwrap();

    // Северная стена здания (ребро вдоль X на z=0): её геометрия лежит на линии z=0,
    // в диапазоне x ∈ [x, x+1], по высоте — по оси Y в пределах этажа.
    let wall = s
        .world
        .edge_element(FloorId(0), GridEdge::along_x(MIN_X + 1, MIN_Z))
        .unwrap();
    assert_eq!(wall.kind, StructureKind::Wall);
    let b = element_bounds(&s.world, floor0, wall).unwrap();
    let c = b.center();
    assert!((c.x - (MIN_X as f32 + 1.5)).abs() < 1e-5, "центр по X");
    assert!((c.z - MIN_Z as f32).abs() < 1e-5, "центр по Z");
    assert!(
        (c.y - FLOOR_HEIGHT_M / 2.0).abs() < 1e-5,
        "центр по Y = середина этажа"
    );

    // Те же координаты, превращённые в ячейки навигации, лежат по обе стороны ребра.
    let [north, south] = GridEdge::along_x(MIN_X + 1, MIN_Z).adjacent_cells();
    assert_eq!(north, GridCell::new(MIN_X + 1, MIN_Z - 1));
    assert_eq!(south, GridCell::new(MIN_X + 1, MIN_Z));
    assert_eq!(
        GridCell::from_world(Vec3::new(c.x, 0.0, c.z + 0.4), 1.0),
        south,
        "+Z — «южнее»"
    );
    assert_eq!(
        GridCell::from_world(Vec3::new(c.x, 0.0, c.z - 0.4), 1.0),
        north
    );

    // Меш того же элемента: самая высокая точка — Y, самая «длинная» ось — X.
    let mesh: MeshData = build_chunk_meshes(&s.world, ChunkCoord::new(0, 0), FloorId(0))
        .into_values()
        .next()
        .unwrap();
    assert!(mesh.bounds().unwrap().max.y <= FLOOR_HEIGHT_M + 1e-5);

    // Спрайт персонажа, стоящего у входа и смотрящего на камеру (камера на +Z), — лицом.
    let at_door = Vec3::new(31.5, 0.0, 0.0);
    let camera = Vec3::new(31.5, 3.0, 12.0);
    assert_eq!(
        select_direction(Vec3::Z, at_door, camera, DirectionSet::Four),
        Direction::South
    );
    assert_eq!(
        select_direction(Vec3::NEG_Z, at_door, camera, DirectionSet::Four),
        Direction::North
    );
    // Идущий в здание через дверь (на юг, +Z) смотрит на камеру, стоящую с южной стороны.
    let path = find_path(
        &s.world,
        FloorId(0),
        OUTSIDE,
        INSIDE,
        &PathOptions::default(),
    )
    .unwrap();
    let step = path[1].min_corner(1.0, 0.0) - path[0].min_corner(1.0, 0.0);
    assert_eq!(step, Vec3::Z, "вход в здание = движение по +Z");
}

#[test]
fn invalid_edits_do_not_corrupt_the_world() {
    let mut s = Slice::build().unwrap();
    let before = s.world.content_hash();
    // Повторная стена на занятом ребре и снос несуществующего объекта.
    let dup = astrocore_world::WorldCommand::PlaceElement(astrocore_world::StructureElement {
        id: ObjectId::from_u128(999_999),
        kind: StructureKind::Wall,
        placement: astrocore_world::Placement::Edge(GridEdge::along_x(MIN_X, MIN_Z)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.25,
    });
    assert!(s.exec(dup).is_err());
    assert!(
        s.exec(astrocore_world::WorldCommand::RemoveElement {
            id: ObjectId::from_u128(404)
        })
        .is_err()
    );
    assert_eq!(s.world.content_hash(), before);
}

/// Пройти `seconds` секунд в направлении `dir` (метры в секунду — скорость ходьбы клиента).
fn walk(
    bw: &astrocore_physics::BoxWorld,
    st: &mut astrocore_physics::CharacterState,
    dir: Vec3,
    seconds: f32,
) {
    let shape = astrocore_physics::CharacterShape::default();
    let dt = 1.0 / 20.0;
    for _ in 0..(seconds / dt).round() as usize {
        astrocore_physics::move_character(bw, st, &shape, dir * 4.0 * dt, dt);
    }
}

#[test]
fn a_character_walks_up_both_staircases_to_the_top_floor() {
    // AC-VS1 п. 2: непрерывные лестницы и корректные коллизии.
    let s = Slice::build().unwrap();
    let bw = astrocore_physics::BoxWorld::from_world(&s.world);
    let mut c = astrocore_physics::CharacterState::at(Vec3::new(30.2, 0.4, 3.5));
    walk(&bw, &mut c, Vec3::X, 1.5); // первая лестница: 1 → 2 этаж
    assert!((c.pos.y - 4.2).abs() < 0.06, "на втором этаже: {:?}", c.pos);
    assert!(c.pos.x > 33.9, "{:?}", c.pos);
    // Вторая лестница стоит в ряду z = 4 и растёт к +X, поэтому подойти к её началу надо
    // в обход высокого конца: по ряду z = 5 на запад, затем на север, в начало марша.
    walk(&bw, &mut c, Vec3::Z, 0.5);
    walk(&bw, &mut c, Vec3::NEG_X, 1.3);
    walk(&bw, &mut c, Vec3::NEG_Z, 0.25);
    walk(&bw, &mut c, Vec3::X, 1.6); // вторая лестница: 2 → 3 этаж
    assert!(
        (c.pos.y - 8.2).abs() < 0.06,
        "на третьем этаже: {:?}",
        c.pos
    );
    assert!(c.grounded);
    // Стены на третьем этаже не пропускают наружу.
    walk(&bw, &mut c, Vec3::X, 2.0);
    assert!(c.pos.x < MAX_X as f32, "{:?}", c.pos);
}

#[test]
fn two_clients_use_the_door_and_see_the_breach_through_the_real_server_core() {
    // AC-VS1 п. 3–5: два клиента, серверная дверь, разрушение стены доходит до обоих.
    use astrocore_network::client::ClientEvent;
    use astrocore_network::server::ServerNetConfig;
    use astrocore_network::testkit::Harness;

    let s = Slice::build().unwrap();
    let cfg = ServerNetConfig {
        spawn: Vec3::new(31.5, 1.0, -1.5),
        ..ServerNetConfig::default()
    };
    let mut h = Harness::with_world(s.world.clone(), cfg);
    let a = h.join("a");
    let b = h.join("b");
    assert_eq!(
        h.client(a).world().unwrap().content_hash(),
        h.client(b).world().unwrap().content_hash()
    );
    let door = h.client(a).door_ids()[0];
    // Закрытая дверь не пускает.
    for _ in 0..30 {
        h.input(a, 0, -1, 0.0); // «назад» при yaw 0 — это +Z, к двери
    }
    assert!(
        h.server.player_position(a).unwrap().z < 0.0,
        "закрытая дверь держит игрока"
    );
    // Рядом с дверью — открываем; оба клиента видят.
    h.send(a, h.client(a).interact_packet(door));
    assert!(h.server.doors().is_open(s.door));
    assert_eq!(h.client(b).door_open(door), Some(true));
    for _ in 0..30 {
        h.input(a, 0, -1, 0.0);
    }
    assert!(
        h.server.player_position(a).unwrap().z > 0.5,
        "вошёл в здание"
    );
    // Разрушение стены на сервере: оба клиента получают операцию, коллизия обновляется.
    h.take_events(a);
    h.take_events(b);
    let id = h
        .server
        .world()
        .edge_element(FloorId(0), BREACH_EDGE)
        .unwrap()
        .id;
    let outs = h
        .server
        .apply_world_command(astrocore_world::WorldCommand::RemoveElement { id })
        .unwrap();
    h.deliver(outs);
    for who in [a, b] {
        assert!(
            h.client(who)
                .world()
                .unwrap()
                .edge_element(FloorId(0), BREACH_EDGE)
                .is_none()
        );
        assert!(
            h.take_events(who)
                .iter()
                .any(|e| matches!(e, ClientEvent::WorldChanged { .. }))
        );
    }
    assert_eq!(
        h.client(a).world().unwrap().content_hash(),
        h.server.world().content_hash()
    );
    // Через пролом (x = 35, z ≈ 2) теперь можно выйти из здания: идём на +X вдоль z = 2.5.
    for _ in 0..50 {
        h.input(a, 0, 1, std::f32::consts::PI); // +Z, к ряду пролома
        if h.server.player_position(a).unwrap().z >= 2.5 {
            break;
        }
    }
    for _ in 0..80 {
        h.input(a, 1, 0, 0.0); // вправо = +X
    }
    assert!(
        h.server.player_position(a).unwrap().x > MAX_X as f32 + 0.3,
        "вышел через пролом: {:?}",
        h.server.player_position(a)
    );
}
