mod package;

pub use package::{PackageError, PackageKind, decode_package, encode_package, package_hash};

use std::collections::{BTreeMap, BTreeSet};

use astrocore_assets::{AssetManifest, ContentHash};
use astrocore_core::{ObjectId, PrototypeId, Revision};
use astrocore_geometry::{MeshData, build_chunk_meshes, collision_boxes};
use astrocore_math::{Aabb, ChunkCoord, GridCell};
use astrocore_spatial::{Portal, RoomKind, Topology};
use astrocore_world::{FloorId, Placement, StructureKind, VerticalLink, World, WorldState};
use serde::{Deserialize, Serialize};

pub const COMPILER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompileError {
    #[error("топология этажа {floor:?}: {reason}")]
    Topology { floor: FloorId, reason: String },
    #[error("материал {0} не описан в манифесте ассетов")]
    UnknownMaterial(PrototypeId),
    #[error("кодирование пакета: {0}")]
    Package(String),
}

/// Меши чанка на этаже, по материалам.
/// Свободно размещённая модель: клиент загружает её по идентификатору.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeformInstance {
    pub id: ObjectId,
    pub model: PrototypeId,
    pub position: [f32; 3],
    pub yaw_rad: f32,
    /// Полуразмеры габаритного ящика (X, высота, Z), метры.
    pub half: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientChunk {
    pub chunk: ChunkCoord,
    pub floor: FloorId,
    pub meshes: Vec<(PrototypeId, MeshData)>,
    pub freeforms: Vec<FreeformInstance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientPayload {
    pub world_hash: u64,
    pub chunks: Vec<ClientChunk>,
    /// Материалы, на которые ссылаются меши (для загрузки текстур клиентом).
    pub materials: Vec<PrototypeId>,
    /// Модели, на которые ссылаются свободные объекты (для загрузки GLB клиентом).
    pub models: Vec<PrototypeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollisionChunk {
    pub chunk: ChunkCoord,
    pub floor: FloorId,
    pub boxes: Vec<(ObjectId, Aabb)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub kind_interior: bool,
    pub cell_count: u32,
    pub min_cell: GridCell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorTopology {
    pub floor: FloorId,
    pub rooms: Vec<RoomSummary>,
    pub portals: Vec<Portal>,
    pub cell_bounds: Option<(GridCell, GridCell)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerPayload {
    pub world: WorldState,
    pub collision: Vec<CollisionChunk>,
    pub topology: Vec<FloorTopology>,
    pub vertical_links: Vec<VerticalLink>,
}

/// Манифест сборки: что именно скомпилировано и из чего.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub compiler_version: u32,
    pub game_id: String,
    pub world_id: String,
    pub world_revision: u64,
    pub world_hash: u64,
    /// Хеш зависимостей: ассеты и внешние входы (например, библиотека префабов).
    pub dependency_hash: ContentHash,
    pub client_hash: ContentHash,
    pub server_hash: ContentHash,
    /// Ассеты без лицензии: блокируют публичный экспорт.
    pub unlicensed_assets: Vec<PrototypeId>,
}

#[derive(Debug, Clone)]
pub struct CompiledMap {
    pub client: Vec<u8>,
    pub server: Vec<u8>,
    pub manifest: BundleManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompileReport {
    pub rebuilt_chunks: usize,
    pub reused_chunks: usize,
}

pub struct CompileInput<'a> {
    pub world: &'a World,
    pub game_id: &'a str,
    pub assets: &'a AssetManifest,
    /// Дополнительные входы сборки (например, хеш библиотеки префабов).
    pub extra_dependencies: &'a [ContentHash],
    /// Требовать, чтобы все материалы были описаны в манифесте ассетов.
    pub require_materials_in_manifest: bool,
}

/// Кэш инкрементальной сборки: чанк × этаж → (ревизия, результат).
#[derive(Debug, Default)]
pub struct CompileCache {
    chunks: BTreeMap<(ChunkCoord, FloorId), (Revision, ClientChunk, CollisionChunk)>,
}

pub fn compile(input: &CompileInput<'_>) -> Result<CompiledMap, CompileError> {
    compile_incremental(input, &mut CompileCache::default()).map(|(m, _)| m)
}

pub fn compile_incremental(
    input: &CompileInput<'_>,
    cache: &mut CompileCache,
) -> Result<(CompiledMap, CompileReport), CompileError> {
    let world = input.world;
    let mut report = CompileReport::default();
    let mut client_chunks = Vec::new();
    let mut collision = Vec::new();
    let mut materials: BTreeSet<PrototypeId> = BTreeSet::new();
    let mut models: BTreeSet<PrototypeId> = BTreeSet::new();
    let mut live = BTreeSet::new();

    let floor_ids: Vec<FloorId> = world.floors().map(|f| f.id).collect();
    for &floor in &floor_ids {
        let def = world.floor(floor).expect("этаж из перечисления существует");
        for chunk in world.populated_chunks(floor) {
            live.insert((chunk, floor));
            let rev = world.chunk_revision(chunk);
            let entry = match cache.chunks.get(&(chunk, floor)) {
                Some((cached, c, k)) if *cached == rev => {
                    report.reused_chunks += 1;
                    (c.clone(), k.clone())
                }
                _ => {
                    report.rebuilt_chunks += 1;
                    let meshes: Vec<(PrototypeId, MeshData)> =
                        build_chunk_meshes(world, chunk, floor)
                            .into_iter()
                            .filter(|(_, m)| !m.is_empty())
                            .collect();
                    let mut boxes = Vec::new();
                    for e in world.elements_in_chunk(chunk, floor) {
                        // Коллизия пишется в чанк-владелец один раз, как и геометрия.
                        let owner = e.placement.touched_cells().into_iter().max().map(|c| {
                            c.chunk(world.config().grid_step_m, world.config().chunk_size_m)
                        });
                        if owner != Some(chunk) {
                            continue;
                        }
                        boxes.extend(
                            collision_boxes(world, def, e)
                                .into_iter()
                                .map(|b| (e.id, b)),
                        );
                    }
                    let step = world.config().grid_step_m;
                    let freeforms: Vec<FreeformInstance> = world
                        .elements_in_chunk(chunk, floor)
                        .filter_map(|e| match (e.kind, e.placement) {
                            (StructureKind::Freeform, Placement::Free(f)) => {
                                Some(FreeformInstance {
                                    id: e.id,
                                    model: e.material.clone(),
                                    position: f.position(step),
                                    yaw_rad: f.yaw_rad(),
                                    half: f.half_mm.map(|h| h as f32 / 1000.0),
                                })
                            }
                            _ => None,
                        })
                        .collect();
                    let c = ClientChunk {
                        chunk,
                        floor,
                        meshes,
                        freeforms,
                    };
                    let k = CollisionChunk {
                        chunk,
                        floor,
                        boxes,
                    };
                    cache
                        .chunks
                        .insert((chunk, floor), (rev, c.clone(), k.clone()));
                    (c, k)
                }
            };
            materials.extend(entry.0.meshes.iter().map(|(m, _)| m.clone()));
            models.extend(entry.0.freeforms.iter().map(|f| f.model.clone()));
            if !entry.0.meshes.is_empty() || !entry.0.freeforms.is_empty() {
                client_chunks.push(entry.0);
            }
            if !entry.1.boxes.is_empty() {
                collision.push(entry.1);
            }
        }
    }
    cache.chunks.retain(|k, _| live.contains(k));

    if input.require_materials_in_manifest
        && let Some(missing) = materials.iter().find(|m| input.assets.get(m).is_none())
    {
        return Err(CompileError::UnknownMaterial(missing.clone()));
    }

    let mut topology = Vec::new();
    for &floor in &floor_ids {
        let t = Topology::build(world, floor).map_err(|e| CompileError::Topology {
            floor,
            reason: e.to_string(),
        })?;
        topology.push(FloorTopology {
            floor,
            rooms: t
                .rooms
                .iter()
                .map(|r| RoomSummary {
                    kind_interior: r.kind == RoomKind::Interior,
                    cell_count: r.cells.len() as u32,
                    min_cell: r.min_cell(),
                })
                .collect(),
            portals: t.portals.clone(),
            cell_bounds: world.floor_cell_bounds(floor),
        });
    }

    let world_hash = world.content_hash();
    let client = ClientPayload {
        world_hash,
        chunks: client_chunks,
        materials: materials.into_iter().collect(),
        models: models.into_iter().collect(),
    };
    let server = ServerPayload {
        world: world.to_state(),
        collision,
        topology,
        vertical_links: world.vertical_links(),
    };
    let client_bytes = encode_package(PackageKind::Client, &client)
        .map_err(|e| CompileError::Package(e.to_string()))?;
    let server_bytes = encode_package(PackageKind::Server, &server)
        .map_err(|e| CompileError::Package(e.to_string()))?;

    let deps: Vec<ContentHash> = std::iter::once(input.assets.dependency_hash())
        .chain(input.extra_dependencies.iter().copied())
        .collect();
    let manifest = BundleManifest {
        compiler_version: COMPILER_VERSION,
        game_id: input.game_id.to_string(),
        world_id: world.id().to_string(),
        world_revision: world.revision().0,
        world_hash,
        dependency_hash: ContentHash::of_hashes(&deps),
        client_hash: package_hash(&client_bytes).expect("пакет не короче хвоста"),
        server_hash: package_hash(&server_bytes).expect("пакет не короче хвоста"),
        unlicensed_assets: input.assets.unlicensed(),
    };
    Ok((
        CompiledMap {
            client: client_bytes,
            server: server_bytes,
            manifest,
        },
        report,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    #[error("хеш {which} пакета не совпадает с манифестом сборки")]
    HashMismatch { which: &'static str },
    #[error("пакеты собраны из разных миров (клиент {client:#x}, сервер {server:#x})")]
    WorldMismatch { client: u64, server: u64 },
    #[error("манифест сборки не соответствует содержимому пакетов")]
    ManifestMismatch,
    #[error("версия компилятора {found} не поддерживается (текущая {COMPILER_VERSION})")]
    CompilerVersion { found: u32 },
    #[error("пакет игры {found:?} нельзя загрузить в игре {expected:?}")]
    GameMismatch { expected: String, found: String },
    #[error("пакет повреждён: {0}")]
    Package(String),
}

/// Проверить, что клиентский и серверный пакеты — две половины одной сборки
/// для ожидаемой игры. Вызывается при загрузке карты и при подключении клиента.
pub fn verify_compatible(
    client: &[u8],
    server: &[u8],
    manifest: &BundleManifest,
    expected_game: &str,
) -> Result<(), CompatError> {
    if manifest.compiler_version != COMPILER_VERSION {
        return Err(CompatError::CompilerVersion {
            found: manifest.compiler_version,
        });
    }
    if manifest.game_id != expected_game {
        return Err(CompatError::GameMismatch {
            expected: expected_game.into(),
            found: manifest.game_id.clone(),
        });
    }
    if package_hash(client) != Some(manifest.client_hash) {
        return Err(CompatError::HashMismatch {
            which: "клиентского",
        });
    }
    if package_hash(server) != Some(manifest.server_hash) {
        return Err(CompatError::HashMismatch {
            which: "серверного",
        });
    }
    let (c, _): (ClientPayload, _) = decode_package(PackageKind::Client, client)
        .map_err(|e| CompatError::Package(e.to_string()))?;
    let (s, _): (ServerPayload, _) = decode_package(PackageKind::Server, server)
        .map_err(|e| CompatError::Package(e.to_string()))?;
    let server_hash = astrocore_world::World::from_state(s.world)
        .map_err(|e| CompatError::Package(e.to_string()))?
        .content_hash();
    if c.world_hash != server_hash {
        return Err(CompatError::WorldMismatch {
            client: c.world_hash,
            server: server_hash,
        });
    }
    if manifest.world_hash != server_hash {
        return Err(CompatError::ManifestMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
