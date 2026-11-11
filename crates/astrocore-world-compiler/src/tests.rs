use astrocore_assets::{AssetEntry, AssetKind, AssetManifest, ContentHash};
use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{Facing, GridCell, GridEdge};
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, StructureElement, StructureKind, World,
    WorldCommand, WorldConfig,
};

use super::*;

fn pid(s: &str) -> PrototypeId {
    PrototypeId::new(s).unwrap()
}

fn manifest(licensed: bool) -> AssetManifest {
    let mut m = AssetManifest::new();
    for (id, bytes) in [("t:stone", b"s" as &[u8]), ("t:door", b"d")] {
        m.insert(AssetEntry {
            id: pid(id),
            kind: AssetKind::Image,
            source: format!("{}.png", id.replace(':', "_")),
            hash: ContentHash::of(bytes),
            license: licensed.then(|| "CC0".to_string()),
            attribution: None,
            transforms: vec![],
        })
        .unwrap();
    }
    m
}

/// Две комнатки, стоящие через границу чанков, лестница и дверь.
fn build_world() -> World {
    let mut w = World::new("compile-test", WorldConfig::default()).unwrap();
    for f in 0..2 {
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(f),
            name: format!("f{f}"),
            base_y: f as f32 * 4.0,
            height: 4.0,
        }))
        .unwrap();
    }
    let mut id = 1u128;
    let mut put = |w: &mut World, kind, placement, floor, mat: &str| {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement,
            floor: FloorId(floor),
            material: pid(mat),
            thickness: 0.2,
        }))
        .unwrap();
        id += 1;
    };
    for x in 30..34 {
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(x, 0)),
            0,
            "t:stone",
        );
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(x, 4)),
            0,
            "t:stone",
        );
    }
    for z in 0..4 {
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(30, z)),
            0,
            "t:stone",
        );
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(34, z)),
            0,
            "t:stone",
        );
    }
    put(
        &mut w,
        StructureKind::Opening(OpeningKind::Door),
        Placement::Edge(GridEdge::along_x(40, 0)),
        0,
        "t:door",
    );
    put(
        &mut w,
        StructureKind::Stair { to: FloorId(1) },
        Placement::Run {
            start: GridCell::new(31, 1),
            dir: Facing::South,
            length: 2,
        },
        0,
        "t:stone",
    );
    w
}

fn input<'a>(w: &'a World, m: &'a AssetManifest) -> CompileInput<'a> {
    CompileInput {
        world: w,
        game_id: "test-game",
        assets: m,
        extra_dependencies: &[],
        require_materials_in_manifest: true,
    }
}

#[test]
fn compilation_is_deterministic() {
    let (w, m) = (build_world(), manifest(true));
    let a = compile(&input(&w, &m)).unwrap();
    let b = compile(&input(&w, &m)).unwrap();
    assert_eq!(a.client, b.client);
    assert_eq!(a.server, b.server);
    assert_eq!(a.manifest, b.manifest);
}

#[test]
fn packages_split_client_and_server_data() {
    let (w, m) = (build_world(), manifest(true));
    let map = compile(&input(&w, &m)).unwrap();
    let (client, ch): (ClientPayload, _) =
        decode_package(PackageKind::Client, &map.client).unwrap();
    let (server, sh): (ServerPayload, _) =
        decode_package(PackageKind::Server, &map.server).unwrap();
    assert_eq!(ch, map.manifest.client_hash);
    assert_eq!(sh, map.manifest.server_hash);

    // Клиенту нужны меши и материалы, но не коллизии и не состояние мира.
    assert!(
        client
            .chunks
            .iter()
            .any(|c| c.chunk == ChunkCoord::new(0, 0))
    );
    assert!(
        client
            .chunks
            .iter()
            .any(|c| c.chunk == ChunkCoord::new(1, 0)),
        "здание пересекает границу чанков"
    );
    assert_eq!(
        client.materials,
        vec![pid("t:stone")],
        "дверь геометрии не даёт: материал не нужен клиенту"
    );
    // Серверу нужны мир, коллизии, топология, связи этажей, но не меши.
    assert_eq!(server.world.elements.len(), w.element_count());
    assert!(!server.collision.is_empty());
    assert_eq!(server.vertical_links.len(), 1);
    assert_eq!(server.topology.len(), 2);
    assert_eq!(
        server.topology[0]
            .rooms
            .iter()
            .filter(|r| r.kind_interior)
            .count(),
        1
    );
    // Дверь в открытом поле соединяет внешнюю область саму с собой (валидатор карты это покажет).
    assert_eq!(server.topology[0].portals.len(), 1);
    assert_eq!(
        server.topology[0].portals[0].a,
        server.topology[0].portals[0].b
    );
    assert_eq!(server.topology[1].rooms.len(), 0, "верхний этаж пуст");
    assert!(map.client.len() != map.server.len());
}

#[test]
fn collision_is_emitted_once_per_element_and_stairs_are_stepped() {
    let (w, m) = (build_world(), manifest(true));
    let map = compile(&input(&w, &m)).unwrap();
    let (server, _): (ServerPayload, _) = decode_package(PackageKind::Server, &map.server).unwrap();
    let mut per_element: BTreeMap<ObjectId, usize> = BTreeMap::new();
    for chunk in &server.collision {
        for (id, _) in &chunk.boxes {
            *per_element.entry(*id).or_default() += 1;
        }
    }
    let stair = w.vertical_links()[0].element;
    assert_eq!(
        per_element[&stair],
        astrocore_geometry::stair_step_count(4.0),
        "по коробке на ступень"
    );
    for (id, n) in per_element {
        if id != stair {
            assert_eq!(n, 1, "{id}");
        }
    }
}

#[test]
fn incremental_build_reuses_unchanged_chunks_and_matches_full_build() {
    let (mut w, m) = (build_world(), manifest(true));
    let mut cache = CompileCache::default();
    let (first, r1) = compile_incremental(&input(&w, &m), &mut cache).unwrap();
    assert_eq!(r1.reused_chunks, 0);
    assert!(r1.rebuilt_chunks >= 2);

    let (again, r2) = compile_incremental(&input(&w, &m), &mut cache).unwrap();
    assert_eq!(
        (r2.rebuilt_chunks, r2.reused_chunks),
        (0, r1.rebuilt_chunks)
    );
    assert_eq!(again.client, first.client);

    // Ломаем стену только в чанке (0,0): чанк (1,0) и этаж 1 берутся из кэша.
    let id = w
        .edge_element(FloorId(0), GridEdge::along_x(31, 0))
        .unwrap()
        .id;
    w.apply(WorldCommand::RemoveElement { id }).unwrap();
    let (inc, r3) = compile_incremental(&input(&w, &m), &mut cache).unwrap();
    assert!(r3.rebuilt_chunks >= 1 && r3.reused_chunks >= 1, "{r3:?}");
    let full = compile(&input(&w, &m)).unwrap();
    assert_eq!(
        inc.client, full.client,
        "инкрементальная сборка эквивалентна полной"
    );
    assert_eq!(inc.server, full.server);
    assert_ne!(inc.manifest.client_hash, first.manifest.client_hash);
    assert_ne!(inc.manifest.world_hash, first.manifest.world_hash);
}

#[test]
fn manifest_reflects_dependencies_and_licenses() {
    let w = build_world();
    let licensed = compile(&input(&w, &manifest(true))).unwrap();
    assert!(licensed.manifest.unlicensed_assets.is_empty());
    let unlicensed = compile(&input(&w, &manifest(false))).unwrap();
    assert_eq!(unlicensed.manifest.unlicensed_assets.len(), 2);

    let extra = [ContentHash::of(b"prefab-library-v1")];
    let m = manifest(true);
    let with_extra = compile(&CompileInput {
        extra_dependencies: &extra,
        ..input(&w, &m)
    })
    .unwrap();
    assert_ne!(
        with_extra.manifest.dependency_hash,
        licensed.manifest.dependency_hash
    );
    assert_eq!(
        with_extra.client, licensed.client,
        "внешние зависимости меняют манифест, а не данные карты"
    );
}

#[test]
fn unknown_materials_are_rejected_when_required() {
    let w = build_world();
    let mut m = AssetManifest::new();
    m.insert(AssetEntry {
        id: pid("t:door"),
        kind: AssetKind::Image,
        source: "d.png".into(),
        hash: ContentHash::of(b"d"),
        license: Some("CC0".into()),
        attribution: None,
        transforms: vec![],
    })
    .unwrap();
    assert_eq!(
        compile(&input(&w, &m)).err(),
        Some(CompileError::UnknownMaterial(pid("t:stone")))
    );
    let lenient = CompileInput {
        require_materials_in_manifest: false,
        ..input(&w, &m)
    };
    assert!(compile(&lenient).is_ok());
}

#[test]
fn matching_halves_are_compatible_and_mismatches_are_refused() {
    let (w, m) = (build_world(), manifest(true));
    let a = compile(&input(&w, &m)).unwrap();
    verify_compatible(&a.client, &a.server, &a.manifest, "test-game").unwrap();

    // Другая игра.
    assert!(matches!(
        verify_compatible(&a.client, &a.server, &a.manifest, "other-game"),
        Err(CompatError::GameMismatch { .. })
    ));
    // Клиент из другой версии мира.
    let mut w2 = build_world();
    let id = w2
        .edge_element(FloorId(0), GridEdge::along_x(31, 0))
        .unwrap()
        .id;
    w2.apply(WorldCommand::RemoveElement { id }).unwrap();
    let b = compile(&input(&w2, &m)).unwrap();
    assert!(matches!(
        verify_compatible(&b.client, &a.server, &a.manifest, "test-game"),
        Err(CompatError::HashMismatch {
            which: "клиентского"
        })
    ));
    let mut forged = a.manifest.clone();
    forged.client_hash = b.manifest.client_hash;
    forged.server_hash = a.manifest.server_hash;
    assert!(matches!(
        verify_compatible(&b.client, &a.server, &forged, "test-game"),
        Err(CompatError::WorldMismatch { .. })
    ));
    // Версия компилятора.
    let mut old = a.manifest.clone();
    old.compiler_version = 0;
    assert!(matches!(
        verify_compatible(&a.client, &a.server, &old, "test-game"),
        Err(CompatError::CompilerVersion { found: 0 })
    ));
}

#[test]
fn damaged_and_hostile_packages_are_rejected_without_panic() {
    let (w, m) = (build_world(), manifest(true));
    let a = compile(&input(&w, &m)).unwrap();

    assert_eq!(
        decode_package::<ClientPayload>(PackageKind::Client, &[]).err(),
        Some(PackageError::Truncated)
    );
    assert_eq!(
        decode_package::<ClientPayload>(PackageKind::Client, &a.server).err(),
        Some(PackageError::WrongKind)
    );
    let mut bad_magic = a.client.clone();
    bad_magic[0] = b'X';
    assert_eq!(
        decode_package::<ClientPayload>(PackageKind::Client, &bad_magic).err(),
        Some(PackageError::BadMagic)
    );
    let mut bad_version = a.client.clone();
    bad_version[5] = 9;
    assert!(matches!(
        decode_package::<ClientPayload>(PackageKind::Client, &bad_version).err(),
        Some(PackageError::UnsupportedVersion(_))
    ));
    let mut huge = a.client.clone();
    huge[9..17].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        decode_package::<ClientPayload>(PackageKind::Client, &huge).err(),
        Some(PackageError::TooLarge(_))
    ));
    let mut flipped = a.client.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0xFF;
    assert_eq!(
        decode_package::<ClientPayload>(PackageKind::Client, &flipped).err(),
        Some(PackageError::HashMismatch)
    );
    let truncated = &a.client[..a.client.len() - 5];
    assert!(decode_package::<ClientPayload>(PackageKind::Client, truncated).is_err());

    // Случайные байты и мутации заголовка не приводят к панике.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..3000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let mut p = a.server.clone();
        let i = (x as usize) % 40.min(p.len());
        p[i] = (x >> 8) as u8;
        let _ = decode_package::<ServerPayload>(PackageKind::Server, &p);
    }
}

#[test]
fn freeform_models_go_to_the_client_as_instances_and_to_the_server_as_collision() {
    use astrocore_world::FreePlacement;
    let mut w = build_world();
    w.apply(WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(777),
        kind: StructureKind::Freeform,
        placement: Placement::Free(FreePlacement::from_position(
            [32.5, 0.2, 2.5],
            1.0,
            90.0,
            [0.5, 1.0, 0.25],
        )),
        floor: FloorId(0),
        material: pid("t:statue"),
        thickness: 0.1,
    }))
    .unwrap();
    let m = manifest(true);
    let map = compile(&CompileInput {
        require_materials_in_manifest: false,
        ..input(&w, &m)
    })
    .unwrap();
    let (client, _): (ClientPayload, _) = decode_package(PackageKind::Client, &map.client).unwrap();
    let (server, _): (ServerPayload, _) = decode_package(PackageKind::Server, &map.server).unwrap();
    assert_eq!(client.models, vec![pid("t:statue")]);
    let inst: Vec<_> = client.chunks.iter().flat_map(|c| &c.freeforms).collect();
    assert_eq!(inst.len(), 1);
    assert!((inst[0].yaw_rad - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    assert!((inst[0].position[0] - 32.5).abs() < 2e-3);
    let boxes: Vec<_> = server
        .collision
        .iter()
        .flat_map(|c| &c.boxes)
        .filter(|(id, _)| id.as_uuid() == ObjectId::from_u128(777).as_uuid())
        .collect();
    assert_eq!(boxes.len(), 1, "ровно один габаритный ящик");
    let b = boxes[0].1;
    assert!(
        (b.size().z - 2.0).abs() < 1e-2 && (b.size().x - 0.5).abs() < 1e-2,
        "повёрнуто на 90°: {b:?}"
    );
    // Статуя не попала в структурные меши (их рисует клиент по модели).
    assert!(!client.materials.contains(&pid("t:statue")));
}
