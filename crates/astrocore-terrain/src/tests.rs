use astrocore_math::ChunkCoord;
use astrocore_math::glam::{Vec2, Vec3};
use astrocore_physics::{BoxWorld, CharacterShape, CharacterState, move_character_on_ground};

use super::*;

fn terrain() -> Terrain {
    Terrain::new(1.0, 16, 0.0).unwrap()
}

fn raise(t: &mut Terrain, c: Vec2, r: f32, a: f32) -> TerrainOp {
    let e = t.brush_raise(c, r, a);
    t.apply(&e).unwrap()
}

#[test]
fn config_is_validated() {
    assert!(Terrain::new(0.0, 16, 0.0).is_err());
    assert!(Terrain::new(f32::NAN, 16, 0.0).is_err());
    assert!(Terrain::new(1.0, 0, 0.0).is_err());
    assert!(Terrain::new(1.0, 5000, 0.0).is_err());
    assert!(Terrain::new(1.0, 16, f32::INFINITY).is_err());
}

#[test]
fn raising_makes_a_smooth_hill_and_lowering_makes_a_pit() {
    let mut t = terrain();
    raise(&mut t, Vec2::new(10.0, 10.0), 6.0, 4.0);
    assert!((t.sample(10.0, 10.0) - 4.0).abs() < 1e-3, "вершина холма");
    assert!(
        t.sample(10.0, 14.0) > 0.0 && t.sample(10.0, 14.0) < 4.0,
        "склон"
    );
    assert_eq!(
        t.sample(10.0, 17.5),
        0.0,
        "за пределами радиуса рельеф не тронут"
    );
    raise(&mut t, Vec2::new(10.0, 10.0), 6.0, -6.0);
    assert!(t.sample(10.0, 10.0) < 0.0, "яма");
}

#[test]
fn bilinear_sampling_matches_the_triangulation() {
    let mut t = terrain();
    let e = TerrainEdit {
        changes: vec![
            (0, 0, 0.0, 0.0),
            (1, 0, 0.0, 2.0),
            (0, 1, 0.0, 4.0),
            (1, 1, 0.0, 6.0),
        ],
    };
    t.apply(&e).unwrap();
    // Вершины точны.
    assert_eq!(t.sample(0.0, 0.0), 0.0);
    assert_eq!(t.sample(1.0, 0.0), 2.0);
    assert_eq!(t.sample(0.0, 1.0), 4.0);
    assert_eq!(t.sample(1.0, 1.0), 6.0);
    // Диагональ (0,0)-(1,1) принадлежит обоим треугольникам: значения совпадают.
    assert!((t.sample(0.5, 0.5) - 3.0).abs() < 1e-5);
    // Внутри треугольников — линейно.
    assert!(
        (t.sample(0.75, 0.25) - (0.0 + 2.0 * 0.75 + 4.0 * 0.25 * 0.0 + (6.0 - 2.0) * 0.25)).abs()
            < 1e-5
    );
    // Отрицательные координаты и далеко от изменённой области.
    assert_eq!(t.sample(-1000.5, 999.25), 0.0);
}

#[test]
fn edits_are_reversible_and_validated() {
    let mut t = terrain();
    let before = t.content_hash();
    let e = t.brush_raise(Vec2::new(5.0, 5.0), 4.0, 3.0);
    assert!(!e.is_empty());
    let op = t.apply(&e).unwrap();
    assert_eq!(op.seq, 1);
    assert_ne!(t.content_hash(), before);
    t.apply(&e.inverse()).unwrap();
    assert_eq!(
        t.content_hash(),
        before,
        "после обратной правки рельеф прежний (даже хеш)"
    );
    assert_eq!(t.revision(), 2);

    // Правка, собранная для другого состояния, отклоняется целиком.
    let stale = TerrainEdit {
        changes: vec![(0, 0, 5.0, 6.0), (1, 1, 0.0, 1.0)],
    };
    assert!(matches!(t.apply(&stale), Err(TerrainError::Stale { .. })));
    assert_eq!(t.height_at_vertex(1, 1), 0.0, "частично не применилось");
    let bad = TerrainEdit {
        changes: vec![(0, 0, 0.0, f32::NAN)],
    };
    assert!(matches!(t.apply(&bad), Err(TerrainError::BadHeight(_))));
    let huge = TerrainEdit {
        changes: vec![(0, 0, 0.0, 1e9)],
    };
    assert!(matches!(t.apply(&huge), Err(TerrainError::BadHeight(_))));
}

#[test]
fn replication_is_ordered_and_idempotent() {
    let mut server = terrain();
    let mut client = terrain();
    let a = raise(&mut server, Vec2::new(3.0, 3.0), 3.0, 2.0);
    let b = raise(&mut server, Vec2::new(8.0, 8.0), 3.0, 1.0);
    assert!(matches!(
        client.apply_replicated(&b),
        Err(TerrainError::Gap {
            expected: 1,
            got: 2
        })
    ));
    assert_eq!(client.apply_replicated(&a), Ok(true));
    assert_eq!(client.apply_replicated(&a), Ok(false), "повтор");
    assert_eq!(client.apply_replicated(&b), Ok(true));
    assert_eq!(client.content_hash(), server.content_hash());
}

#[test]
fn dirty_chunks_include_neighbours_for_border_vertices() {
    let mut t = terrain(); // чанки по 16 ячеек
    t.take_dirty();
    // Вершина (16, 16) лежит на углу четырёх чанков.
    t.apply(&TerrainEdit {
        changes: vec![(16, 16, 0.0, 1.0)],
    })
    .unwrap();
    let mut d = t.take_dirty();
    d.sort();
    assert_eq!(
        d,
        vec![
            ChunkCoord::new(0, 0),
            ChunkCoord::new(0, 1),
            ChunkCoord::new(1, 0),
            ChunkCoord::new(1, 1)
        ]
    );
    // Внутренняя вершина — один чанк.
    t.apply(&TerrainEdit {
        changes: vec![(5, 5, 0.0, 1.0)],
    })
    .unwrap();
    assert_eq!(t.take_dirty(), vec![ChunkCoord::new(0, 0)]);
    // Отрицательные координаты.
    t.apply(&TerrainEdit {
        changes: vec![(-1, -1, 0.0, 1.0)],
    })
    .unwrap();
    assert!(t.take_dirty().contains(&ChunkCoord::new(-1, -1)));
}

#[test]
fn flatten_rect_builds_a_foundation_with_a_blended_shoulder() {
    let mut t = terrain();
    raise(&mut t, Vec2::new(20.0, 20.0), 12.0, 5.0); // холм
    let target = 3.0;
    let e = t.flatten_rect(Vec2::new(17.0, 17.0), Vec2::new(23.0, 23.0), target, 4.0);
    t.apply(&e).unwrap();
    for (x, z) in [(17.0, 17.0), (20.0, 20.0), (23.0, 23.0), (18.5, 22.0)] {
        assert!(
            (t.sample(x, z) - target).abs() < 1e-3,
            "площадка ровная в ({x},{z})"
        );
    }
    // Склон вокруг монотонно приближается к исходному рельефу, без ступеньки.
    let step_max = (0..40)
        .map(|i| {
            (t.sample(23.0 + i as f32 * 0.25, 20.0) - t.sample(23.0 + (i + 1) as f32 * 0.25, 20.0))
                .abs()
        })
        .fold(0.0f32, f32::max);
    assert!(step_max < 1.0, "плавный переход: {step_max}");
    // Повторное выравнивание площадку не трогает (обочина дотягивается дальше — это нормально).
    let again = t.flatten_rect(Vec2::new(17.0, 17.0), Vec2::new(23.0, 23.0), target, 4.0);
    assert!(
        again
            .changes
            .iter()
            .all(|&(x, z, _, _)| !((17..=23).contains(&x) && (17..=23).contains(&z)))
    );
}

#[test]
fn roads_follow_the_path_and_interpolate_height() {
    let mut t = terrain();
    raise(&mut t, Vec2::new(30.0, 0.0), 14.0, 6.0);
    let path = [
        Vec2::new(0.0, 0.0),
        Vec2::new(30.0, 0.0),
        Vec2::new(60.0, 0.0),
    ];
    let start_h = t.sample(0.0, 0.0);
    let end_h = t.sample(60.0, 0.0);
    let e = t.road(&path, 4.0, 3.0);
    t.apply(&e).unwrap();
    let mut prev = start_h;
    for i in 0..=12 {
        let x = i as f32 * 5.0;
        let h = t.sample(x, 0.0);
        let expected = start_h + (end_h - start_h) * (x / 60.0);
        assert!((h - expected).abs() < 0.3, "x={x}: {h} vs {expected}");
        assert!((h - prev).abs() < 1.0, "дорога без скачков");
        prev = h;
    }
    // Ширина полотна: поперёк дороги внутри 2 м — ровно, дальше обочины — рельеф возвращается.
    assert!((t.sample(30.0, 1.5) - t.sample(30.0, 0.0)).abs() < 0.1);
    assert!(
        t.sample(30.0, 7.0) > t.sample(30.0, 0.0) + 0.5,
        "холм за обочиной остался"
    );
    assert!(t.road(&path[..1], 4.0, 1.0).is_empty());
}

#[test]
fn slope_normal_and_walkability() {
    let mut t = terrain();
    // Клин: высота растёт на 1 м на каждый метр по X → 45°.
    let changes: Vec<_> = (-5..=5)
        .flat_map(|x| (-5..=5).map(move |z| (x, z, 0.0, x as f32)))
        .collect();
    t.apply(&TerrainEdit { changes }).unwrap();
    assert!((t.slope_deg(0.3, 0.3) - 45.0).abs() < 0.5);
    let n = t.normal(0.3, 0.3);
    assert!(n.x < 0.0 && n.y > 0.0 && (n.length() - 1.0).abs() < 1e-5);
    assert!(t.is_walkable(0.0, 0.0, 50.0) && !t.is_walkable(0.0, 0.0, 30.0));
    assert!(t.is_walkable(100.0, 100.0, 1.0), "ровное место");
    t.set_water_level(Some(2.0));
    assert!(
        !t.is_walkable(-3.0, 0.0, 80.0),
        "под водой глубже 0.3 м идти нельзя"
    );
    assert!(t.is_submerged(Vec3::new(-3.0, 1.0, 0.0)) && !t.is_submerged(Vec3::new(4.0, 6.0, 0.0)));
    t.set_water_level(Some(f32::NAN));
    assert_eq!(t.water_level(), None);
}

#[test]
fn meshes_agree_with_sampling_and_chunks_have_no_seams() {
    let mut t = terrain();
    raise(&mut t, Vec2::new(16.0, 8.0), 9.0, 3.0); // холм на границе чанков (x = 16)
    for chunk in [ChunkCoord::new(0, 0), ChunkCoord::new(1, 0)] {
        let m = t.build_chunk_mesh(chunk);
        assert_eq!(m.positions.len(), 17 * 17);
        assert_eq!(m.triangle_count(), 16 * 16 * 2);
        for p in m.positions.iter().step_by(7) {
            assert!(
                (t.sample(p[0], p[2]) - p[1]).abs() < 1e-4,
                "вершина меша лежит на поверхности"
            );
        }
        for tri in m.indices.chunks(3) {
            let v: Vec<Vec3> = tri
                .iter()
                .map(|&i| Vec3::from_array(m.positions[i as usize]))
                .collect();
            assert!(
                (v[1] - v[0]).cross(v[2] - v[0]).y > 0.0,
                "треугольники смотрят вверх"
            );
        }
    }
    // Общая граница чанков: вершины и нормали совпадают.
    let (a, b) = (
        t.build_chunk_mesh(ChunkCoord::new(0, 0)),
        t.build_chunk_mesh(ChunkCoord::new(1, 0)),
    );
    for z in 0..=16usize {
        let (va, vb) = (z * 17 + 16, z * 17);
        assert_eq!(a.positions[va], b.positions[vb]);
        assert_eq!(a.normals[va], b.normals[vb]);
    }
    assert!(t.populated_chunks().contains(&ChunkCoord::new(0, 0)));
}

#[test]
fn state_roundtrip_and_hostile_state() {
    let mut t = terrain();
    raise(&mut t, Vec2::new(2.0, -3.0), 5.0, 2.5);
    t.set_water_level(Some(0.5));
    let json = serde_json::to_string(&t.to_state()).unwrap();
    let back = Terrain::from_state(serde_json::from_str(&json).unwrap()).unwrap();
    assert_eq!(back.content_hash(), t.content_hash());
    assert_eq!(
        (back.revision(), back.water_level()),
        (t.revision(), Some(0.5))
    );
    let mut bad = t.to_state();
    bad.vertices.push(bad.vertices[0]);
    assert!(matches!(
        Terrain::from_state(bad),
        Err(TerrainError::Corrupt(_))
    ));
    let mut bad = t.to_state();
    bad.vertices[0].2 = f32::NAN;
    assert!(matches!(
        Terrain::from_state(bad),
        Err(TerrainError::BadHeight(_))
    ));
    let mut bad = t.to_state();
    bad.resolution_m = -1.0;
    assert!(matches!(
        Terrain::from_state(bad),
        Err(TerrainError::Config(_))
    ));
}

#[test]
fn characters_walk_on_hills_and_stop_at_cliffs() {
    let mut t = terrain();
    // Пологий холм: подъём 3 м на 12 м (≈14°).
    raise(&mut t, Vec2::new(30.0, 0.0), 14.0, 3.0);
    let bw = BoxWorld::default();
    let shape = CharacterShape::default();
    let mut s = CharacterState::at(Vec3::new(14.0, 5.0, 0.0));
    let dt = 1.0 / 20.0;
    for _ in 0..40 {
        move_character_on_ground(&bw, &t, &mut s, &shape, Vec3::ZERO, dt);
    }
    assert!(
        s.grounded && (s.pos.y - t.sample(14.0, 0.0)).abs() < 0.02,
        "приземлился на землю: {:?}",
        s.pos
    );
    for _ in 0..80 {
        move_character_on_ground(&bw, &t, &mut s, &shape, Vec3::X * 4.0 * dt, dt);
    }
    assert!(s.pos.x > 28.0, "поднялся на холм: {:?}", s.pos);
    assert!(s.pos.y > 2.5 && (s.pos.y - t.sample(s.pos.x, s.pos.z)).abs() < 0.05);

    // Обрыв: ступенька 5 м не проходится.
    let mut cliff = terrain();
    let changes: Vec<_> = (20..=40)
        .flat_map(|x| (-6..=6).map(move |z| (x, z, 0.0, 5.0)))
        .collect();
    cliff.apply(&TerrainEdit { changes }).unwrap();
    let mut c = CharacterState::at(Vec3::new(15.0, 0.0, 0.0));
    for _ in 0..200 {
        move_character_on_ground(&bw, &cliff, &mut c, &shape, Vec3::X * 4.0 * dt, dt);
    }
    assert!(
        c.pos.x < 20.5 && c.pos.y < 1.0,
        "крутой подъём блокирует движение: {:?}",
        c.pos
    );
}
