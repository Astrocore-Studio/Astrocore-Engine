//! Загрузка карт в формате SS14. Образец — `Robust.Shared/EntitySerialization`
//! (`EntityDeserializer`, `MapChunkSerializer`) @ 1afd692.
//!
//! Файл карты — документ YAML или RON: таблица тайлов `tilemap` и сущности по прототипам,
//! у каждой — номер в файле и компоненты, отличающиеся от прототипа. Этажи — расширение
//! Astrocore: поле `z` у `Transform` и у чанка грида; в картах SS14 его нет, и всё грузится
//! на этаж 0.

use std::collections::{BTreeMap, HashMap};

use astrocore_data::{DataNode, from_node};
use astrocore_ecs::{LifecycleWorldExt, MetaData};
use astrocore_math::{Angle, IVec2, Vec2};
use astrocore_prototypes::{EntityOverrides, PrototypeWorldExt};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use crate::container::{CONTAINER_COMPONENT, ContainerKind, ContainerManager, ContainerWorldExt};
use crate::coords::{CHUNK_SIZE, MapId, TileCoord};
use crate::grid::{Tile, TileDefinitions};
use crate::map::MapWorldExt;
use crate::transform::{Placement, Transform, TransformWorldExt};

/// Наименьшая и наибольшая версии формата файла карты, которые читает загрузчик.
pub const MAP_FORMAT_RANGE: (u32, u32) = (6, 7);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapLoadError {
    #[error("файл карты: {0}")]
    Format(String),
    #[error("версия формата карты {0} не поддерживается")]
    Version(u32),
}

/// Переименования прототипов сущностей (`migration.yml` SS14): старый идентификатор →
/// новый; `None` — сущности со старым идентификатором удаляются при загрузке. Ресурс `World`.
#[derive(bevy_ecs::resource::Resource, Debug, Default, Clone)]
pub struct EntityMigrations {
    pub renames: HashMap<String, Option<String>>,
}

impl EntityMigrations {
    /// Читает документ `migration.yml`: отображение старых идентификаторов на новые.
    pub fn from_node(node: &DataNode) -> Self {
        let renames = node
            .as_map()
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(old, new)| (old.to_owned(), new.as_str().map(str::to_owned)))
            .collect();
        Self { renames }
    }

    /// Действующий идентификатор прототипа; `None` — сущность удалена.
    fn resolve<'a>(&'a self, id: &'a str) -> Option<&'a str> {
        let mut current = id;
        // Цепочки переименований проходятся до конца; цикл обрывается по счётчику.
        for _ in 0..8 {
            match self.renames.get(current) {
                Some(Some(next)) if !next.is_empty() => current = next,
                Some(_) => return None,
                None => break,
            }
        }
        Some(current)
    }
}

/// Итог загрузки карты.
#[derive(Debug, Default)]
pub struct MapLoadReport {
    /// Созданные карты.
    pub maps: Vec<(Entity, MapId)>,
    /// Созданные гриды.
    pub grids: Vec<Entity>,
    /// Сколько сущностей создано.
    pub entities: usize,
    /// Номер сущности в файле → сущность мира.
    pub uids: HashMap<u32, Entity>,
    /// Прототип, которого нет в игре → сколько сущностей пропущено.
    pub unknown_prototypes: BTreeMap<String, usize>,
    /// Сколько сущностей удалено по таблице переименований.
    pub migrated_away: usize,
    /// Тайлы из таблицы файла, определений которых нет в игре.
    pub unknown_tiles: Vec<String>,
    /// Сущности, пропущенные из-за ошибок в данных: описание.
    pub errors: Vec<String>,
}

/// Декодирует Base64 (стандартный алфавит, с дополнением `=` или без него).
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Тайлы чанка из его записи в файле.
fn read_chunk(
    chunk: &DataNode,
    tile_ids: &HashMap<i32, u16>,
) -> Result<Vec<(TileCoord, Tile)>, String> {
    let index: IVec2 = chunk
        .get("ind")
        .ok_or("у чанка нет поля ind".to_owned())
        .and_then(|node| from_node(node).map_err(|e| e.to_string()))?;
    let level: i16 = match chunk.get("z") {
        Some(node) => from_node(node).map_err(|e| e.to_string())?,
        None => 0,
    };
    let version: u32 = match chunk.get("version") {
        Some(node) => from_node(node).map_err(|e| e.to_string())?,
        None => 1,
    };
    let bytes = chunk
        .get("tiles")
        .and_then(DataNode::as_str)
        .and_then(decode_base64)
        .ok_or("тайлы чанка не читаются как Base64".to_owned())?;
    // Запись тайла: номер, флаги, вариант и, с версии 7, поворот.
    let id_bytes = if version < 6 { 2 } else { 4 };
    let record = id_bytes + if version >= 7 { 3 } else { 2 };
    let count = (CHUNK_SIZE * CHUNK_SIZE) as usize;
    if bytes.len() < count * record {
        return Err(format!(
            "в чанке {index} {} байт вместо {}",
            bytes.len(),
            count * record
        ));
    }
    let mut tiles = Vec::new();
    for (position, raw) in bytes.chunks_exact(record).take(count).enumerate() {
        let id = if id_bytes == 2 {
            i32::from(u16::from_le_bytes([raw[0], raw[1]]))
        } else {
            i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
        };
        let Some(&type_id) = tile_ids.get(&id) else {
            return Err(format!("в чанке {index} тайл {id} вне таблицы тайлов"));
        };
        if type_id == 0 {
            continue;
        }
        let coord = TileCoord::new(
            index.x * CHUNK_SIZE + position as i32 % CHUNK_SIZE,
            index.y * CHUNK_SIZE + position as i32 / CHUNK_SIZE,
            level,
        );
        tiles.push((
            coord,
            Tile {
                type_id,
                flags: raw[id_bytes],
                variant: raw[id_bytes + 1],
                rotation: if version >= 7 { raw[id_bytes + 2] } else { 0 },
            },
        ));
    }
    Ok(tiles)
}

/// Запись сущности файла, разобранная до создания.
struct Pending<'a> {
    uid: u32,
    prototype: String,
    components: Vec<(&'a str, &'a DataNode)>,
    missing: Vec<&'a str>,
    line: u32,
}

#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TransformData {
    pos: Vec2,
    rot: Angle,
    z: i16,
    parent: Option<u32>,
    anchored: Option<bool>,
    no_rot: Option<bool>,
}

fn component<'a>(pending: &Pending<'a>, name: &str) -> Option<&'a DataNode> {
    pending
        .components
        .iter()
        .find(|(component, _)| *component == name)
        .map(|(_, node)| *node)
}

/// Загрузка карт на `World`.
pub trait MapLoadWorldExt {
    /// Создаёт карты, гриды и сущности из разобранного файла карты. `initialize: false` —
    /// карты остаются на паузе и без `MapInit`, как при маппинге.
    fn load_map(
        &mut self,
        root: &DataNode,
        initialize: bool,
    ) -> Result<MapLoadReport, MapLoadError>;
}

impl MapLoadWorldExt for World {
    fn load_map(
        &mut self,
        root: &DataNode,
        initialize: bool,
    ) -> Result<MapLoadReport, MapLoadError> {
        let mut report = MapLoadReport::default();
        let format: u32 = root
            .get("meta")
            .and_then(|meta| meta.get("format"))
            .ok_or_else(|| MapLoadError::Format("нет поля meta.format".into()))
            .and_then(|node| from_node(node).map_err(|e| MapLoadError::Format(e.to_string())))?;
        if format < MAP_FORMAT_RANGE.0 || format > MAP_FORMAT_RANGE.1 {
            return Err(MapLoadError::Version(format));
        }

        // Таблица тайлов файла → номера определений игры.
        let mut tile_ids: HashMap<i32, u16> = HashMap::new();
        self.init_resource::<TileDefinitions>();
        if let Some(tilemap) = root.get("tilemap").and_then(DataNode::as_map) {
            for (key, name) in tilemap.iter() {
                let (Ok(id), Some(name)) = (key.parse::<i32>(), name.as_str()) else {
                    return Err(MapLoadError::Format(format!("запись tilemap {key}")));
                };
                let mut definitions = self.resource_mut::<TileDefinitions>();
                let type_id = match definitions.by_id(name) {
                    Some(definition) => definition.type_id,
                    None => {
                        // Карта грузится и с неизвестным тайлом: он станет виден в отчёте.
                        report.unknown_tiles.push(name.to_owned());
                        definitions.register(name, None, 1)
                    }
                };
                tile_ids.insert(id, type_id);
            }
        }

        // Сущности файла.
        let groups = root
            .get("entities")
            .and_then(DataNode::as_seq)
            .ok_or_else(|| MapLoadError::Format("нет списка entities".into()))?;
        let migrations = self
            .get_resource::<EntityMigrations>()
            .cloned()
            .unwrap_or_default();
        let mut pending: Vec<Pending<'_>> = Vec::new();
        for group in groups {
            let prototype = group.get("proto").and_then(DataNode::as_str).unwrap_or("");
            let Some(prototype) = migrations.resolve(prototype) else {
                report.migrated_away += group
                    .get("entities")
                    .and_then(DataNode::as_seq)
                    .map_or(0, <[DataNode]>::len);
                continue;
            };
            for entry in group
                .get("entities")
                .and_then(DataNode::as_seq)
                .unwrap_or_default()
            {
                let Some(uid) = entry.get("uid").and_then(|n| from_node::<u32>(n).ok()) else {
                    report
                        .errors
                        .push(format!("строка {}: у сущности нет номера uid", entry.line));
                    continue;
                };
                let components = entry
                    .get("components")
                    .and_then(DataNode::as_seq)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|node| Some((node.type_name()?, node)))
                    .collect();
                let missing = entry
                    .get("missingComponents")
                    .and_then(DataNode::as_seq)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(DataNode::as_str)
                    .collect();
                pending.push(Pending {
                    uid,
                    prototype: prototype.to_owned(),
                    components,
                    missing,
                    line: entry.line,
                });
            }
        }

        // Создание: компоненты из прототипа и файла, без места и без инициализации.
        // Положение, карта, грид и содержимое контейнеров задаются ниже по своим правилам.
        const HANDLED: [&str; 4] = ["Transform", "MetaData", "Map", "MapGrid"];
        let mut created: Vec<(usize, Entity)> = Vec::new();
        for (index, entry) in pending.iter().enumerate() {
            let cleaned: Vec<(&str, DataNode)> = entry
                .components
                .iter()
                .filter(|(name, _)| !HANDLED.contains(name))
                .map(|(name, node)| {
                    let mut copy = (*node).clone();
                    copy.tag = None;
                    if let Some(map) = copy.as_map_mut() {
                        map.remove("type");
                    }
                    (*name, copy)
                })
                .collect();
            let overrides = EntityOverrides {
                components: cleaned.iter().map(|(name, node)| (*name, node)).collect(),
                missing: entry.missing.clone(),
            };
            let entity = if entry.prototype.is_empty() {
                let entity = self.spawn_entity();
                let registry = self.resource::<astrocore_ecs::ComponentRegistry>();
                let parsed: Vec<_> = overrides
                    .components
                    .iter()
                    .filter_map(|(name, node)| {
                        let info = registry.get(name)?;
                        Some((info.id, info.parse(node)))
                    })
                    .collect();
                for (id, result) in parsed {
                    match result {
                        Ok(parsed) => {
                            parsed.component.insert_clone(self, entity);
                            self.notify_comp_added(entity, id);
                        }
                        Err(error) => report.errors.push(format!(
                            "строка {}: сущность {}: {error}",
                            entry.line, entry.uid
                        )),
                    }
                }
                entity
            } else {
                match self.spawn_prototype_with(&entry.prototype, &overrides) {
                    Ok(entity) => entity,
                    Err(astrocore_prototypes::SpawnError::UnknownPrototype(id)) => {
                        *report.unknown_prototypes.entry(id).or_default() += 1;
                        continue;
                    }
                    Err(error) => {
                        report.errors.push(format!(
                            "строка {}: сущность {}: {error}",
                            entry.line, entry.uid
                        ));
                        continue;
                    }
                }
            };
            if self.get::<Transform>(entity).is_none() {
                self.entity_mut(entity).insert(Transform::default());
            }
            if let Some(meta) = component(entry, "MetaData") {
                let name = meta
                    .get("name")
                    .and_then(DataNode::as_str)
                    .map(str::to_owned);
                let description = meta
                    .get("desc")
                    .and_then(DataNode::as_str)
                    .map(str::to_owned);
                if let Some(mut data) = self.get_mut::<MetaData>(entity) {
                    data.name = name.or(data.name.take());
                    data.description = description.or(data.description.take());
                }
            }
            report.uids.insert(entry.uid, entity);
            created.push((index, entity));
        }
        report.entities = created.len();

        // Карты и гриды.
        for &(index, entity) in &created {
            let entry = &pending[index];
            if component(entry, "Map").is_some() {
                let maps = self.get_resource_or_insert_with(crate::map::Maps::default);
                let id = MapId(maps.iter().map(|(id, _)| id.0).max().unwrap_or(0) + 1);
                // Карта для маппинга стоит на паузе; её сущности встают на паузу при размещении.
                self.adopt_map(entity, id, initialize, !initialize);
                self.set_entity_paused(entity, !initialize);
                report.maps.push((entity, id));
            }
        }
        for &(index, entity) in &created {
            let entry = &pending[index];
            let Some(grid) = component(entry, "MapGrid") else {
                continue;
            };
            self.ensure_grid(entity);
            report.grids.push(entity);
            let chunks = grid.get("chunks").and_then(DataNode::as_map);
            for (_, chunk) in chunks.into_iter().flat_map(|map| map.iter()) {
                match read_chunk(chunk, &tile_ids) {
                    Ok(tiles) => {
                        self.set_tiles(entity, tiles);
                    }
                    Err(error) => report.errors.push(format!(
                        "строка {}: грид {}: {error}",
                        chunk.line, entry.uid
                    )),
                }
            }
        }

        // Положение: родитель раньше потомка.
        let mut placement: HashMap<Entity, (TransformData, u32)> = HashMap::new();
        for &(index, entity) in &created {
            let entry = &pending[index];
            let data = match component(entry, "Transform") {
                Some(node) => match from_node::<TransformData>(node) {
                    Ok(data) => data,
                    Err(error) => {
                        report
                            .errors
                            .push(format!("сущность {}: положение: {error}", entry.uid));
                        TransformData::default()
                    }
                },
                None => TransformData::default(),
            };
            placement.insert(entity, (data, entry.uid));
        }
        let mut order: Vec<Entity> = Vec::with_capacity(created.len());
        let mut placed: HashMap<Entity, bool> = HashMap::new();
        fn visit(
            entity: Entity,
            placement: &HashMap<Entity, (TransformData, u32)>,
            uids: &HashMap<u32, Entity>,
            placed: &mut HashMap<Entity, bool>,
            order: &mut Vec<Entity>,
            depth: usize,
        ) {
            if placed.contains_key(&entity) || depth > 64 {
                return;
            }
            placed.insert(entity, true);
            if let Some(parent) = placement
                .get(&entity)
                .and_then(|(data, _)| data.parent)
                .and_then(|uid| uids.get(&uid))
            {
                visit(*parent, placement, uids, placed, order, depth + 1);
            }
            order.push(entity);
        }
        for &(_, entity) in &created {
            visit(entity, &placement, &report.uids, &mut placed, &mut order, 0);
        }
        for &entity in &order {
            let Some((data, uid)) = placement.get(&entity) else {
                continue;
            };
            let Some(parent_uid) = data.parent else {
                continue;
            };
            let Some(&parent) = report.uids.get(&parent_uid) else {
                report
                    .errors
                    .push(format!("сущность {uid}: родителя {parent_uid} нет в файле"));
                continue;
            };
            let current = self.get::<Transform>(entity);
            let anchored = data
                .anchored
                .unwrap_or_else(|| current.is_some_and(Transform::anchored));
            let no_rot = data
                .no_rot
                .unwrap_or_else(|| current.is_some_and(Transform::no_rot));
            if let Err(error) = self.set_placement(
                entity,
                Placement {
                    parent: Some(parent),
                    position: data.pos,
                    rotation: data.rot,
                    level: data.z,
                    anchored,
                    no_rot,
                },
            ) {
                report.errors.push(format!("сущность {uid}: {error}"));
            }
        }

        // Инициализация в том же порядке: родитель раньше потомка.
        for &entity in &order {
            let map_init = initialize && self.is_on_initialized_map(entity);
            self.initialize_and_start(entity, map_init);
        }
        // Содержимое контейнеров.
        for &(index, entity) in &created {
            let entry = &pending[index];
            let Some(containers) = component(entry, CONTAINER_COMPONENT)
                .and_then(|node| node.get("containers"))
                .and_then(DataNode::as_map)
            else {
                continue;
            };
            for (id, container) in containers.iter() {
                let kind = match container.type_tag() {
                    Some("ContainerSlot") => ContainerKind::Slot,
                    _ => ContainerKind::List,
                };
                if self.get::<ContainerManager>(entity).is_none()
                    || self.container(entity, id).is_none()
                {
                    self.ensure_container(entity, id, kind);
                }
                // Слот хранит одну сущность в поле `ent`, контейнер — список `ents`.
                let single = container.get("ent").into_iter();
                let many = container
                    .get("ents")
                    .and_then(DataNode::as_seq)
                    .unwrap_or_default()
                    .iter();
                for uid in single.chain(many).filter_map(|n| from_node::<u32>(n).ok()) {
                    let Some(&item) = report.uids.get(&uid) else {
                        continue;
                    };
                    if let Err(error) = self.force_insert_into_container(item, entity, id) {
                        report
                            .errors
                            .push(format!("сущность {uid} в контейнере {id}: {error}"));
                    }
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("AQID").unwrap(), [1, 2, 3]);
        assert_eq!(decode_base64("AQIDBA==").unwrap(), [1, 2, 3, 4]);
        assert_eq!(decode_base64("AQIDBA").unwrap(), [1, 2, 3, 4]);
        assert!(decode_base64("AQ*D").is_none());
    }
}
