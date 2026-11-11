use std::time::Instant;

use astrocore_assets::AssetManifest;
use astrocore_core::{ObjectId, PrototypeId};
use astrocore_geometry::build_chunk_meshes;
use astrocore_math::glam::Vec3;
use astrocore_math::{GridCell, GridEdge};
use astrocore_navigation::{NavGrid, PathOptions, find_path_3d};
use astrocore_network::testkit::Harness;
use astrocore_physics::{BoxWorld, CharacterShape, CharacterState, move_character};
use astrocore_protocol::{ClientMessage, ServerMessage, decode, encode};
use astrocore_spatial::Topology;
use astrocore_world::{
    FloorDef, FloorId, Placement, StructureElement, StructureKind, World, WorldCommand, WorldConfig,
};
use astrocore_world_compiler::{CompileCache, CompileInput, compile_incremental};

#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: &'static str,
    pub iterations: u64,
    pub total_ms: f64,
}

impl BenchResult {
    pub fn per_iter_us(&self) -> f64 {
        self.total_ms * 1000.0 / self.iterations.max(1) as f64
    }
}

fn timed(name: &'static str, iterations: u64, mut f: impl FnMut()) -> BenchResult {
    let start = Instant::now();
    f();
    BenchResult {
        name,
        iterations,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
    }
}

/// Квартал `blocks × blocks` комнат 6×6 клеток с перегородками и стенами; все — на одном этаже.
pub fn city_world(blocks: i32) -> World {
    let mut w = World::new("bench", WorldConfig::default()).expect("конфигурация верна");
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .expect("этаж");
    let mat = PrototypeId::new("bench:wall").expect("валидный ID");
    let mut id = 1u128;
    let mut put = |w: &mut World, kind, placement| {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement,
            floor: FloorId(0),
            material: mat.clone(),
            thickness: 0.2,
        }))
        .expect("элемент");
        id += 1;
    };
    for bx in 0..blocks {
        for bz in 0..blocks {
            let (x0, z0) = (bx * 8, bz * 8);
            for i in 0..6 {
                put(
                    &mut w,
                    StructureKind::Wall,
                    Placement::Edge(GridEdge::along_x(x0 + i, z0)),
                );
                put(
                    &mut w,
                    StructureKind::Wall,
                    Placement::Edge(GridEdge::along_x(x0 + i, z0 + 6)),
                );
                put(
                    &mut w,
                    StructureKind::Wall,
                    Placement::Edge(GridEdge::along_z(x0, z0 + i)),
                );
                put(
                    &mut w,
                    StructureKind::Wall,
                    Placement::Edge(GridEdge::along_z(x0 + 6, z0 + i)),
                );
                for j in 0..6 {
                    put(
                        &mut w,
                        StructureKind::Floor,
                        Placement::Cell(GridCell::new(x0 + i, z0 + j)),
                    );
                }
            }
        }
    }
    w.take_dirty();
    w
}

/// Набор измерений. `blocks` — размер города (по умолчанию 8 → 64 здания ≈ 4000 элементов).
pub fn run(blocks: i32) -> Vec<BenchResult> {
    let mut out = Vec::new();
    let mut world = None;
    out.push(timed("мир: построить город", 1, || {
        world = Some(city_world(blocks))
    }));
    let world = world.expect("мир построен");
    let n = world.element_count() as u64;

    let chunks = world.populated_chunks(FloorId(0));
    out.push(timed(
        "геометрия: меши всех чанков",
        chunks.len() as u64,
        || {
            for c in &chunks {
                std::hint::black_box(build_chunk_meshes(&world, *c, FloorId(0)));
            }
        },
    ));
    out.push(timed(
        "топология: комнаты и порталы этажа",
        1,
        || {
            std::hint::black_box(Topology::build(&world, FloorId(0)).expect("топология"));
        },
    ));
    let mut nav = None;
    out.push(timed(
        "навигация: построить кэш тайлов",
        1,
        || nav = Some(NavGrid::build(&world)),
    ));
    let nav = nav.expect("кэш построен");
    let span = blocks * 8;
    let (a, b) = (
        (FloorId(0), GridCell::new(-1, -1)),
        (FloorId(0), GridCell::new(span, span)),
    );
    out.push(timed(
        "навигация: путь через весь город (кэш)",
        20,
        || {
            for _ in 0..20 {
                let _ = std::hint::black_box(find_path_3d(
                    &nav,
                    a,
                    b,
                    &PathOptions {
                        node_limit: 1_000_000,
                        ..Default::default()
                    },
                ));
            }
        },
    ));
    let mut bw = None;
    out.push(timed(
        "физика: запечь коллизию",
        1,
        || bw = Some(BoxWorld::from_world(&world)),
    ));
    let bw = bw.expect("коллизия запечена");
    out.push(timed(
        "физика: шаги персонажа",
        10_000,
        || {
            let shape = CharacterShape::default();
            let mut c = CharacterState::at(Vec3::new(3.0, 1.0, 3.0));
            for i in 0..10_000 {
                let dir = if (i / 40) % 2 == 0 { Vec3::X } else { Vec3::Z };
                move_character(&bw, &mut c, &shape, dir * 0.2, 0.05);
            }
            std::hint::black_box(c);
        },
    ));
    let assets = AssetManifest::new();
    let input = CompileInput {
        world: &world,
        game_id: "bench",
        assets: &assets,
        extra_dependencies: &[],
        require_materials_in_manifest: false,
    };
    let mut cache = CompileCache::default();
    out.push(timed(
        "компилятор: полная сборка карты",
        1,
        || {
            std::hint::black_box(compile_incremental(&input, &mut cache).expect("сборка"));
        },
    ));
    out.push(timed(
        "компилятор: повторная (всё из кэша)",
        1,
        || {
            std::hint::black_box(compile_incremental(&input, &mut cache).expect("сборка"));
        },
    ));
    out.push(timed(
        "протокол: кодирование+разбор 10 000 сообщений",
        10_000,
        || {
            let msg = ClientMessage::Input {
                seq: 1,
                move_x: 1,
                move_z: 1,
                yaw: 0.5,
            };
            for _ in 0..10_000 {
                let bytes = encode(&msg).expect("кодируется");
                std::hint::black_box(decode::<ClientMessage>(&bytes).expect("разбирается"));
            }
        },
    ));
    out.push(timed(
        "протокол: кодирование снимка мира частями",
        1,
        || {
            let parts =
                astrocore_network::snapshot::split(&world.to_state()).expect("снимок делится");
            for p in &parts {
                let _ = std::hint::black_box(encode::<ServerMessage>(p));
            }
        },
    ));
    out.push(timed(
        "сервер: 50 игроков × 100 тиков",
        5_000,
        || {
            let mut h = Harness::with_world(city_world(2), Harness::default_cfg());
            let ids: Vec<_> = (0..50).map(|i| h.join(&format!("p{i}"))).collect();
            for _ in 0..100 {
                for &id in &ids {
                    if let Some(p) = h
                        .clients
                        .get_mut(&id)
                        .and_then(|c| c.input_packet(1, 0, 0.0))
                    {
                        let outs = h.server.on_packet(id, &p);
                        h.deliver(outs);
                    }
                }
                h.tick();
            }
        },
    ));
    let _ = n;
    out
}

pub fn format_table(blocks: i32, results: &[BenchResult], elements: usize) -> String {
    let mut s = format!("Город {blocks}×{blocks} кварталов, элементов: {elements}\n");
    s.push_str(&format!(
        "{:<56} {:>10} {:>12} {:>14}\n",
        "сценарий", "итераций", "всего, мс", "на итерацию, мкс"
    ));
    for r in results {
        s.push_str(&format!(
            "{:<56} {:>10} {:>12.2} {:>14.2}\n",
            r.name,
            r.iterations,
            r.total_ms,
            r.per_iter_us()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_suite_runs_and_reports_every_scenario() {
        let r = run(2);
        assert!(r.len() >= 10);
        assert!(r.iter().all(|b| b.total_ms >= 0.0 && b.iterations >= 1));
        let table = format_table(2, &r, city_world(2).element_count());
        assert!(table.contains("компилятор: полная сборка карты"));
        // Повторная сборка из кэша не медленнее полной (после прогрева, с запасом на шум).
        let full = r
            .iter()
            .find(|b| b.name.contains("полная сборка"))
            .unwrap()
            .total_ms;
        let cached = r
            .iter()
            .find(|b| b.name.contains("всё из кэша"))
            .unwrap()
            .total_ms;
        assert!(cached <= full * 2.0 + 5.0, "{cached} vs {full}");
    }
}
