use super::*;

fn cube(n: u16, color: u8) -> VoxelModel {
    let mut m = VoxelModel::new([n, n, n], vec![[255, 0, 0, 255], [0, 0, 255, 255]]).unwrap();
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                m.set(x, y, z, color).unwrap();
            }
        }
    }
    m
}

#[test]
fn a_model_rejects_bad_sizes_palettes_and_indices() {
    assert!(VoxelModel::new([0, 1, 1], vec![[0; 4]]).is_err());
    assert!(VoxelModel::new([257, 1, 1], vec![[0; 4]]).is_err());
    assert!(VoxelModel::new([1, 1, 1], vec![]).is_err());
    let mut m = VoxelModel::new([2, 2, 2], vec![[1, 2, 3, 255]]).unwrap();
    assert!(m.set(0, 0, 0, 2).is_err(), "цвета 2 нет");
    assert!(m.set(2, 0, 0, 1).is_err(), "за пределами");
    m.set(1, 1, 1, 1).unwrap();
    assert_eq!(m.color(1, 1, 1), Some([1, 2, 3, 255]));
    assert_eq!(m.get(-1, 0, 0), 0, "снаружи пусто");
    assert_eq!(m.pivot(), [1.0, 0.0, 1.0], "опора — центр основания");
}

#[test]
fn the_avox_round_trip_keeps_everything() {
    let mut m = cube(3, 1);
    m.set(1, 1, 1, 2).unwrap();
    m.set(0, 2, 0, 0).unwrap();
    m.set_voxels_per_meter(16).unwrap();
    m.set_pivot([1.5, 0.0, 0.5]);
    let bytes = encode(&m);
    assert_eq!(decode(&bytes).unwrap(), m);
    assert!(bytes.len() < 27 + 64, "серии сжимают: {} байт", bytes.len());
}

#[test]
fn damaged_avox_files_are_refused_without_panics() {
    let good = encode(&cube(2, 1));
    assert!(decode(b"NOPE").is_err());
    for cut in 0..good.len() {
        assert!(decode(&good[..cut]).is_err(), "обрезан на {cut}");
    }
    let mut extra = good.clone();
    extra.push(0);
    assert!(decode(&extra).is_err(), "лишний хвост");
    // Индекс за палитрой.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] = 9;
    assert!(decode(&bad).is_err());
    // Мусор не роняет декодер.
    let mut seed = 0x1234_5678u32;
    for _ in 0..2000 {
        let mut b = good.clone();
        for _ in 0..3 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (seed as usize) % b.len();
            b[i] = (seed >> 24) as u8;
        }
        let _ = decode(&b);
    }
}

#[test]
fn a_single_voxel_is_six_quads_one_voxel_wide() {
    let mut m = VoxelModel::new([1, 1, 1], vec![[10, 20, 30, 255]]).unwrap();
    m.set(0, 0, 0, 1).unwrap();
    let mesh = build_mesh(&m);
    assert_eq!(mesh.quad_count(), 6);
    let xs: Vec<f32> = mesh.positions.iter().map(|p| p[0]).collect();
    let span =
        xs.iter().cloned().fold(f32::MIN, f32::max) - xs.iter().cloned().fold(f32::MAX, f32::min);
    assert!((span - 1.0 / 32.0).abs() < 1e-6, "ширина {span}");
    assert!(
        mesh.positions.iter().all(|p| p[1] >= 0.0),
        "опора на основании"
    );
}

#[test]
fn equal_faces_merge_and_inner_faces_vanish() {
    // Сплошной куб 4×4×4 одного цвета — ровно шесть больших граней.
    assert_eq!(build_mesh(&cube(4, 1)).quad_count(), 6);
    // Другой цвет в середине не виден снаружи и граней не добавляет.
    let mut m = cube(4, 1);
    m.set(1, 1, 1, 2).unwrap();
    assert_eq!(build_mesh(&m).quad_count(), 6);
    // Другой цвет на грани режет только её.
    let mut m = cube(4, 1);
    m.set(0, 0, 0, 2).unwrap();
    assert!(build_mesh(&m).quad_count() > 6);
}

#[test]
fn faces_point_outward_and_wind_counterclockwise() {
    let mesh = build_mesh(&cube(2, 1));
    for q in mesh.indices.chunks(3) {
        let [a, b, c] = [q[0], q[1], q[2]].map(|i| mesh.positions[i as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let want = mesh.normals[q[0] as usize];
        let dot = n[0] * want[0] + n[1] * want[1] + n[2] * want[2];
        assert!(dot > 0.0, "треугольник смотрит внутрь");
    }
}

/// Минимальный `.vox`: модель 2×3×4 (MagicaVoxel, Z вверх), два вокселя и палитра.
fn vox_file() -> Vec<u8> {
    let chunk = |id: &[u8; 4], body: Vec<u8>| {
        let mut c = id.to_vec();
        c.extend((body.len() as u32).to_le_bytes());
        c.extend(0u32.to_le_bytes());
        c.extend(body);
        c
    };
    let mut size = Vec::new();
    for v in [2u32, 3, 4] {
        size.extend(v.to_le_bytes());
    }
    let mut xyzi = 2u32.to_le_bytes().to_vec();
    xyzi.extend([0, 0, 0, 1]); // низ, «перед» по Y MagicaVoxel
    xyzi.extend([1, 2, 3, 2]); // верх, «зад»
    let mut rgba = Vec::new();
    for i in 0..256u32 {
        rgba.extend([i as u8, 0, 0, 255]);
    }
    let children = [
        chunk(b"SIZE", size),
        chunk(b"XYZI", xyzi),
        chunk(b"RGBA", rgba),
    ]
    .concat();
    let mut out = b"VOX ".to_vec();
    out.extend(150u32.to_le_bytes());
    out.extend(b"MAIN");
    out.extend(0u32.to_le_bytes());
    out.extend((children.len() as u32).to_le_bytes());
    out.extend(children);
    out
}

#[test]
fn magicavoxel_files_are_turned_to_y_up() {
    let m = import_vox(&vox_file()).unwrap();
    assert_eq!(m.size(), [2, 4, 3], "Z MagicaVoxel стал высотой");
    assert_eq!(m.filled(), 2);
    // (0,0,0) MagicaVoxel → (0, 0, size_y-1) у нас; (1,2,3) → (1, 3, 0).
    assert_eq!(m.get(0, 0, 2), 1);
    assert_eq!(m.get(1, 3, 0), 2);
    assert_eq!(
        m.color(1, 3, 0),
        Some([1, 0, 0, 255]),
        "цвет 2 — второй в палитре"
    );
}

#[test]
fn damaged_vox_files_are_refused_without_panics() {
    let good = vox_file();
    assert!(import_vox(b"VOX").is_err());
    for cut in 8..good.len() {
        let _ = import_vox(&good[..cut]);
    }
    let mut seed = 7u32;
    for _ in 0..2000 {
        let mut b = good.clone();
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let i = (seed as usize) % b.len();
        b[i] = (seed >> 24) as u8;
        let _ = import_vox(&b);
    }
}
