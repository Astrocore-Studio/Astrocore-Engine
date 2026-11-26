//! Сохранение карты в формат SS14. Образец — `Robust.Shared/EntitySerialization`
//! (`EntitySerializer`, `MapChunkSerializer`) @ 1afd692.
//!
//! Сущность пишется прототипом и компонентами, которые отличаются от прототипа: значение
//! компонента сравнивается с компонентом прототипа поле за полем, совпавшие поля не пишутся,
//! компонент без отличий не пишется вовсе. Компоненты прототипа, которых у сущности нет, —
//! в `missingComponents`. Компоненты, которых игра не понимает, пишутся как были прочитаны
//! (`MapPreserved`), как и непонятные игре поля известных компонентов. Компонент, которого
//! нет в прототипе, пишется полями, отличными от значений по умолчанию.
//!
//! Отличия от Robust ради устойчивых файлов: сущности сохраняют номера из файла, откуда
//! загружены (`MapFileEntity`), тайлы — номера из его `tilemap`; новые получают следующие
//! свободные. Сущности вне карт (`nullspace`, например чанки декалей SS14) сохраняются с
//! картой своего файла. Пустые контейнеры не пишутся. Этажи — расширение Astrocore: поле `z`
//! у `Transform` и у чанка, если он не на этаже 0.

use std::collections::{BTreeMap, HashMap};

use astrocore_data::{DataMap, DataNode, Value};
use astrocore_ecs::{ComponentRegistry, LifeStage, MetaData, Paused};
use astrocore_prototypes::Prototypes;
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use bevy_ecs::world::World;

use crate::container::{CONTAINER_COMPONENT, ContainerKind, ContainerManager};
use crate::coords::CHUNK_SIZE;
use crate::grid::{MapGrid, TileDefinitions};
use crate::loader::{MapFileEntity, MapFileTilemap, MapPreserved};
use crate::map::Map;
use crate::transform::Transform;

/// Версия формата, которую пишет сохранение.
pub const MAP_FORMAT_VERSION: u32 = 7;
/// Версия записи тайлов чанка: номер, флаги, вариант, поворот.
const CHUNK_VERSION: u32 = 7;

/// Компоненты, которые сохранение пишет само, а не через реестр.
const HANDLED: [&str; 5] = [
    "Transform",
    "MetaData",
    "Map",
    "MapGrid",
    CONTAINER_COMPONENT,
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapSaveError {
    #[error("сущность не карта")]
    NotAMap,
    #[error("карта уже инициализирована: сохраняются только карты для маппинга")]
    Initialized,
}

/// Настройки сохранения (`SerializationOptions`).
#[derive(Debug, Clone, Copy, Default)]
pub struct MapSaveOptions {
    /// Сохранить и инициализированную карту (у сущностей будет `mapInit: true`).
    pub allow_initialized: bool,
}

/// Итог сохранения: документ и число сущностей.
#[derive(Debug, Clone)]
pub struct MapSave {
    pub document: DataNode,
    pub entities: usize,
}

fn plain(value: impl ToString) -> DataNode {
    DataNode::plain(value.to_string())
}

fn number(value: f32) -> String {
    // Как `float.ToString(InvariantCulture)`: без лишних нулей.
    let text = format!("{value}");
    if text == "-0" { "0".to_owned() } else { text }
}

/// Строка, как её пишет SS14: без кавычек, если их не требует сам YAML или значение не
/// читается как пустое, логическое или число.
fn text(value: &str) -> DataNode {
    let special = value.is_empty()
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off"
        )
        || value.parse::<f64>().is_ok();
    if special {
        DataNode::string(value)
    } else {
        plain(value)
    }
}

/// Base64 со стандартным алфавитом и дополнением `=`.
fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(value >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Сохранение карт на `World`.
pub trait MapSaveWorldExt {
    /// Записывает карту со всеми её сущностями в документ формата SS14.
    fn save_map(&mut self, map: Entity, options: MapSaveOptions) -> Result<MapSave, MapSaveError>;
}

struct Saver<'w> {
    world: &'w World,
    registry: &'w ComponentRegistry,
    prototypes: Option<&'w Prototypes>,
    uids: HashMap<Entity, u32>,
    /// Компоненты прототипа в записанном виде.
    cache: HashMap<String, HashMap<String, DataNode>>,
    /// Номер тайла в игре → номер в файле.
    tiles: BTreeMap<u16, u32>,
    /// Компоненты со значениями по умолчанию в записанном виде.
    defaults: HashMap<String, Option<DataNode>>,
    /// Номера тайлов файла, из которого загружена карта.
    file_tiles: BTreeMap<u16, u32>,
    /// Последний номер, выданный тайлу, которого в том файле не было.
    next_tile: u32,
}

impl Saver<'_> {
    /// Сущности карты: родитель раньше потомков, у потомков — порядок сущностей.
    fn collect(&self, root: Entity, out: &mut Vec<Entity>) {
        let savable = self
            .world
            .get::<MetaData>(root)
            .and_then(|meta| meta.prototype.clone())
            .and_then(|id| self.prototypes?.entity(&id).map(|proto| proto.save))
            .unwrap_or(true);
        if !savable {
            return;
        }
        out.push(root);
        let mut children: Vec<Entity> = self
            .world
            .get::<Children>(root)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();
        // Порядок не зависит от порядка создания: содержимое контейнеров — по контейнеру и
        // месту в нём, остальное — по прототипу и положению.
        let key = |child: &Entity| {
            let slot = self
                .world
                .get::<crate::container::InContainer>(*child)
                .and_then(|inside| {
                    let index = self
                        .world
                        .get::<ContainerManager>(inside.owner)?
                        .containers
                        .get(&*inside.id)?
                        .entities()
                        .iter()
                        .position(|e| e == child)?;
                    Some((inside.id.to_string(), index))
                });
            let prototype = self
                .world
                .get::<MetaData>(*child)
                .and_then(|meta| meta.prototype.clone())
                .map(|p| p.to_string())
                .unwrap_or_default();
            let (x, y, rotation, z) =
                self.world
                    .get::<Transform>(*child)
                    .map_or((0.0, 0.0, 0.0, 0), |t| {
                        let p = t.local_position();
                        (p.x, p.y, t.local_rotation().0, t.level())
                    });
            let file = self.world.get::<MapFileEntity>(*child).map(|f| f.uid);
            (
                slot,
                prototype,
                z,
                ordered(y),
                ordered(x),
                ordered(rotation),
                file,
                *child,
            )
        };
        children.sort_by_cached_key(key);
        for child in children {
            self.collect(child, out);
        }
    }

    fn prototype_cache(&mut self, id: &str) -> Option<&HashMap<String, DataNode>> {
        if !self.cache.contains_key(id) {
            let proto = self.prototypes?.entity(id)?.clone();
            let mut written = HashMap::new();
            for component in &proto.components {
                let node = component
                    .parsed
                    .as_ref()
                    .and_then(|parsed| {
                        self.registry
                            .get(&component.name)?
                            .write_value(parsed.as_ref())
                    })
                    .and_then(Result::ok)
                    .unwrap_or_else(|| (*component.node).clone());
                written.insert(component.name.to_string(), node);
            }
            self.cache.insert(id.to_owned(), written);
        }
        self.cache.get(id)
    }

    /// Компонент со значениями по умолчанию в записанном виде.
    fn default_of(&mut self, info: &astrocore_ecs::ComponentInfo) -> Option<DataNode> {
        self.defaults
            .entry(info.name.to_string())
            .or_insert_with(|| {
                let parsed = info.parse(&DataNode::map(DataMap::new())).ok()?;
                info.write_value(parsed.component.as_ref())?.ok()
            })
            .clone()
    }

    fn uid(&self, entity: Entity) -> Option<u32> {
        self.uids.get(&entity).copied()
    }

    /// Значение флага `Transform` прототипа (`anchored`, `noRot`).
    fn prototype_flag(&self, prototype: Option<&str>, field: &str) -> bool {
        prototype
            .and_then(|id| self.prototypes?.entity(id))
            .and_then(|proto| proto.component("Transform").map(|c| c.node.clone()))
            .and_then(|node| {
                node.get(field)
                    .and_then(DataNode::as_str)
                    .map(|v| v.eq_ignore_ascii_case("true"))
            })
            .unwrap_or(false)
    }

    fn transform(&self, entity: Entity, prototype: Option<&str>) -> DataNode {
        let mut map = DataMap::new();
        if let Some(parent) = self
            .world
            .get::<bevy_ecs::hierarchy::ChildOf>(entity)
            .and_then(|c| self.uid(c.parent()))
        {
            map.insert("parent", plain(parent));
        }
        if let Some(transform) = self.world.get::<Transform>(entity) {
            let position = transform.local_position();
            // Как у Robust: `rot` перед `pos`.
            let rotation = transform.local_rotation().0;
            if rotation != 0.0 {
                map.insert(
                    "rot",
                    plain(format!("{} rad", transform.local_rotation().radians_text())),
                );
            }
            if position.x != 0.0 || position.y != 0.0 {
                map.insert(
                    "pos",
                    plain(format!("{},{}", number(position.x), number(position.y))),
                );
            }
            if transform.level() != 0 {
                map.insert("z", plain(transform.level()));
            }
            // Флаг пишется, когда он не такой, как у прототипа (`alwaysWrite` Robust).
            let flag = |value: bool| plain(if value { "True" } else { "False" });
            if transform.no_rot() != self.prototype_flag(prototype, "noRot") {
                map.insert("noRot", flag(transform.no_rot()));
            }
            if transform.anchored() != self.prototype_flag(prototype, "anchored") {
                map.insert("anchored", flag(transform.anchored()));
            }
        }
        DataNode::map(map)
    }

    fn meta(&self, entity: Entity) -> DataNode {
        let mut map = DataMap::new();
        if let Some(meta) = self.world.get::<MetaData>(entity) {
            if let Some(name) = &meta.name {
                map.insert("name", text(name));
            }
            if let Some(description) = &meta.description {
                map.insert("desc", text(description));
            }
        }
        DataNode::map(map)
    }

    fn grid(&mut self, grid: &MapGrid) -> DataNode {
        let mut chunks = DataMap::new();
        let mut list: Vec<_> = grid
            .chunks()
            .filter(|(_, chunk)| chunk.filled() > 0)
            .collect();
        list.sort_by_key(|(pos, _)| (pos.z, pos.y, pos.x));
        let count = (CHUNK_SIZE * CHUNK_SIZE) as usize;
        for (pos, chunk) in list {
            let mut bytes = Vec::with_capacity(count * 7);
            for tile in chunk.tiles().iter() {
                let id = match self.tiles.get(&tile.type_id) {
                    Some(id) => *id,
                    None => {
                        let id = self
                            .file_tiles
                            .get(&tile.type_id)
                            .copied()
                            .unwrap_or_else(|| {
                                self.next_tile += 1;
                                self.next_tile
                            });
                        self.tiles.insert(tile.type_id, id);
                        id
                    }
                };
                bytes.extend_from_slice(&(id as i32).to_le_bytes());
                bytes.push(tile.flags);
                bytes.push(tile.variant);
                bytes.push(tile.rotation);
            }
            let mut node = DataMap::new();
            node.insert("ind", plain(format!("{},{}", pos.x, pos.y)));
            if pos.z != 0 {
                node.insert("z", plain(pos.z));
            }
            node.insert("tiles", plain(encode_base64(&bytes)));
            node.insert("version", plain(CHUNK_VERSION));
            let key = if pos.z == 0 {
                format!("{},{}", pos.x, pos.y)
            } else {
                format!("{},{},{}", pos.x, pos.y, pos.z)
            };
            chunks.insert(key, DataNode::map(node));
        }
        let mut map = DataMap::new();
        map.insert("chunks", DataNode::map(chunks));
        DataNode::map(map)
    }

    /// Контейнеры с сохраняемым содержимым. Пустые не пишутся: системы создают их заново
    /// при инициализации сущности, а файл карты остаётся коротким, как у карт SS14.
    fn containers(&self, manager: &ContainerManager) -> Option<DataNode> {
        let mut containers = DataMap::new();
        let mut ids: Vec<&String> = manager.containers.keys().collect();
        ids.sort();
        for id in ids {
            let container = &manager.containers[id];
            if !container
                .entities()
                .iter()
                .any(|e| self.uids.contains_key(e))
            {
                continue;
            }
            let mut node = DataMap::new();
            let tag = match container.kind {
                ContainerKind::Slot => {
                    let ent = container.entities().first().and_then(|e| self.uid(*e));
                    node.insert("ent", ent.map_or_else(DataNode::null, plain));
                    "type:ContainerSlot"
                }
                ContainerKind::List => {
                    let ents = container
                        .entities()
                        .iter()
                        .filter_map(|e| self.uid(*e))
                        .map(plain)
                        .collect();
                    node.insert("ents", DataNode::seq(ents));
                    "type:Container"
                }
            };
            node.insert(
                "showEnts",
                plain(if container.show_ents { "True" } else { "False" }),
            );
            node.insert(
                "occludes",
                plain(if container.occludes_light {
                    "True"
                } else {
                    "False"
                }),
            );
            containers.insert(id.clone(), DataNode::map(node).with_tag(tag));
        }
        if containers.is_empty() {
            return None;
        }
        let mut map = DataMap::new();
        map.insert("containers", DataNode::map(containers));
        Some(DataNode::map(map))
    }

    /// Компоненты сущности (`SerializeComponents`).
    fn components(
        &mut self,
        entity: Entity,
        prototype: Option<&str>,
    ) -> (Vec<DataNode>, Vec<String>) {
        let proto_cache = prototype.and_then(|id| self.prototype_cache(id).cloned());
        // У сущности с прототипом пустые `MetaData` и `Transform` не пишутся: в них нет
        // отличий от прототипа.
        let mut written: Vec<(String, DataNode)> = [
            ("MetaData", self.meta(entity)),
            ("Transform", self.transform(entity, prototype)),
        ]
        .into_iter()
        .filter(|(_, node)| prototype.is_none() || node.as_map().is_some_and(|m| !m.is_empty()))
        .map(|(name, node)| (name.to_owned(), node))
        .collect();
        if let Some(map) = self.world.get::<Map>(entity) {
            let mut node = DataMap::new();
            if map.paused {
                node.insert("mapPaused", plain("True"));
            }
            written.push(("Map".to_owned(), DataNode::map(node)));
        }
        if let Some(grid) = self.world.get::<MapGrid>(entity) {
            let node = self.grid(grid);
            written.push(("MapGrid".to_owned(), node));
        }
        let preserved = self
            .world
            .get::<MapPreserved>(entity)
            .cloned()
            .unwrap_or_default();
        for info in self.registry.iter() {
            if HANDLED.contains(&&*info.name) {
                continue;
            }
            let Some(Ok(node)) = info.write(self.world, entity) else {
                continue;
            };
            // Поля компонента из файла, которых игра не читает, дописываются как были.
            let extra = preserved
                .fields
                .iter()
                .find(|(name, _)| *name == *info.name)
                .map(|(_, fields)| fields);
            let node = match proto_cache
                .as_ref()
                .and_then(|cache| cache.get(&*info.name))
            {
                Some(proto) => match (except(&node, proto), extra) {
                    (Some(diff), _) => diff,
                    (None, Some(_)) => DataNode::map(DataMap::new()),
                    (None, None) => continue,
                },
                // Компонента нет в прототипе: пишутся поля, отличные от значений по
                // умолчанию (`alwaysWrite: false`), но сам компонент — всегда.
                None => match self.default_of(info) {
                    Some(default) => {
                        except(&node, &default).unwrap_or_else(|| DataNode::map(DataMap::new()))
                    }
                    None => node,
                },
            };
            let mut node = csharp_bools(node);
            if let (Some(extra), Some(map)) = (extra, node.as_map_mut()) {
                for (key, value) in extra.iter() {
                    if !map.contains_key(key) {
                        map.insert(key, value.clone());
                    }
                }
            }
            written.push((info.name.to_string(), node));
        }
        if let Some(node) = self
            .world
            .get::<ContainerManager>(entity)
            .and_then(|manager| self.containers(manager))
        {
            written.push((CONTAINER_COMPONENT.to_owned(), node));
        }
        let mut components: Vec<DataNode> = written
            .into_iter()
            .map(|(name, node)| {
                let mut map = DataMap::new();
                map.insert("type", plain(name));
                if let Some(fields) = node.as_map() {
                    for (key, value) in fields.iter() {
                        map.insert(key, value.clone());
                    }
                }
                DataNode::map(map)
            })
            .collect();
        components.extend(preserved.components);
        // Компоненты прототипа, которых у сущности нет.
        let mut missing = Vec::new();
        if let Some(proto) = prototype.and_then(|id| self.prototypes?.entity(id)) {
            for component in &proto.components {
                let Some(info) = self.registry.get(&component.name) else {
                    continue;
                };
                let present = self
                    .world
                    .get_entity(entity)
                    .is_ok_and(|e| e.contains_id(info.id));
                if !present && !HANDLED.contains(&&*component.name) {
                    missing.push(component.name.to_string());
                }
            }
        }
        missing.extend(preserved.missing);
        (components, missing)
    }
}

/// Логические значения так, как их пишет C# (`True`, `False`), чтобы файл не менялся от
/// пересохранения в Robust и обратно.
fn csharp_bools(mut node: DataNode) -> DataNode {
    fn walk(node: &mut DataNode) {
        match &mut node.value {
            Value::Scalar(scalar) if !scalar.quoted => match scalar.text.as_str() {
                "true" => scalar.text = "True".into(),
                "false" => scalar.text = "False".into(),
                _ => {}
            },
            Value::Scalar(_) => {}
            Value::Seq(items) => items.iter_mut().for_each(walk),
            Value::Map(map) => map.values_mut().for_each(walk),
        }
    }
    walk(&mut node);
    node
}

/// Ключ сортировки строк, близкий к `StringComparer.InvariantCulture`, которым Robust
/// упорядочивает прототипы в файле: сначала без учёта регистра (знаки раньше цифр, цифры
/// раньше букв), при равенстве строчная буква раньше заглавной.
fn invariant_order(text: &str) -> (Vec<(u8, char)>, Vec<bool>) {
    let primary = text
        .chars()
        .map(|c| {
            let class = if c.is_alphabetic() {
                2
            } else if c.is_numeric() {
                1
            } else {
                0
            };
            (class, c.to_lowercase().next().unwrap_or(c))
        })
        .collect();
    let tertiary = text.chars().map(char::is_uppercase).collect();
    (primary, tertiary)
}

/// Ключ сортировки для `f32` с тем же порядком, что у `f32::total_cmp`.
fn ordered(value: f32) -> i32 {
    let bits = value.to_bits() as i32;
    bits ^ ((((bits >> 31) as u32) >> 1) as i32)
}

/// Поля компонента, которые отличаются от прототипа (`MappingDataNode.Except`): `None` —
/// отличий нет.
fn except(node: &DataNode, prototype: &DataNode) -> Option<DataNode> {
    let (Some(fields), Some(proto)) = (node.as_map(), prototype.as_map()) else {
        return (node != prototype).then(|| node.clone());
    };
    let mut diff = DataMap::new();
    for (key, value) in fields.iter() {
        if proto.get(key) != Some(value) {
            diff.insert(key, value.clone());
        }
    }
    (!diff.is_empty()).then(|| DataNode::map(diff))
}

impl MapSaveWorldExt for World {
    fn save_map(&mut self, map: Entity, options: MapSaveOptions) -> Result<MapSave, MapSaveError> {
        let Some(component) = self.get::<Map>(map) else {
            return Err(MapSaveError::NotAMap);
        };
        if component.initialized && !options.allow_initialized {
            return Err(MapSaveError::Initialized);
        }
        self.init_resource::<TileDefinitions>();
        let world: &World = self;
        let mut saver = Saver {
            world,
            registry: world.resource::<ComponentRegistry>(),
            prototypes: world.get_resource::<Prototypes>(),
            uids: HashMap::new(),
            cache: HashMap::new(),
            tiles: BTreeMap::new(),
            defaults: HashMap::new(),
            file_tiles: BTreeMap::new(),
            next_tile: 0,
        };
        if let Some(file) = world.get::<MapFileTilemap>(map) {
            saver.file_tiles = file.0.clone();
        }
        // Пустота всегда под номером 0, как в SS14.
        saver.file_tiles.entry(0).or_insert(0);
        saver.next_tile = saver.file_tiles.values().max().copied().unwrap_or(0);
        let mut entities = Vec::new();
        saver.collect(map, &mut entities);
        // Сущности вне карт, загруженные с ней из одного файла.
        let mut outside: Vec<(u32, Entity)> = {
            let mut query = world
                .try_query::<(Entity, &MapFileEntity)>()
                .expect("компонент зарегистрирован");
            query
                .iter(world)
                .filter(|(entity, file)| {
                    file.nullspace_of == Some(map)
                        && world.get::<bevy_ecs::hierarchy::ChildOf>(*entity).is_none()
                })
                .map(|(entity, file)| (file.uid, entity))
                .collect()
        };
        outside.sort_unstable();
        let mut nullspace = Vec::new();
        for (_, entity) in outside {
            let start = entities.len();
            saver.collect(entity, &mut entities);
            if entities.len() > start {
                nullspace.push(entity);
            }
        }
        // Номера из файла остаются за сущностями; новые сущности получают номера после них.
        let mut taken = std::collections::HashSet::new();
        for &entity in &entities {
            if let Some(file) = world.get::<MapFileEntity>(entity)
                && file.uid != 0
                && taken.insert(file.uid)
            {
                saver.uids.insert(entity, file.uid);
            }
        }
        let mut next = taken.iter().max().copied().unwrap_or(0);
        for &entity in &entities {
            saver.uids.entry(entity).or_insert_with(|| {
                next += 1;
                next
            });
        }

        let mut maps = Vec::new();
        let mut grids = Vec::new();
        let mut by_prototype: BTreeMap<String, Vec<(u32, DataNode)>> = BTreeMap::new();
        for &entity in &entities {
            let uid = saver.uids[&entity];
            if world.get::<Map>(entity).is_some() {
                maps.push(plain(uid));
            }
            if world.get::<MapGrid>(entity).is_some() {
                grids.push(plain(uid));
            }
            let meta = world.get::<MetaData>(entity);
            let prototype = meta
                .and_then(|m| m.prototype.clone())
                .map(|p| p.to_string());
            let mut node = DataMap::new();
            node.insert("uid", plain(uid));
            let paused = world.get::<Paused>(entity).is_some();
            if meta.is_some_and(|m| m.stage == LifeStage::MapInitialized) {
                node.insert("mapInit", plain("true"));
                if paused {
                    node.insert("paused", plain("true"));
                }
            } else if !paused {
                node.insert("paused", plain("false"));
            }
            let (components, missing) = saver.components(entity, prototype.as_deref());
            if !components.is_empty() {
                node.insert("components", DataNode::seq(components));
            }
            if !missing.is_empty() {
                node.insert(
                    "missingComponents",
                    DataNode::seq(missing.into_iter().map(plain).collect()),
                );
            }
            by_prototype
                .entry(prototype.unwrap_or_default())
                .or_default()
                .push((uid, DataNode::map(node)));
        }

        let definitions = world.resource::<TileDefinitions>();
        let mut tilemap = DataMap::new();
        // Как в SS14: пустота первой, остальные по имени.
        let mut tile_list: Vec<(bool, String, u32)> = saver
            .tiles
            .iter()
            .map(|(type_id, id)| {
                let name = definitions
                    .get(*type_id)
                    .map_or_else(|| crate::grid::SPACE_TILE.to_owned(), |d| d.id.clone());
                (name != crate::grid::SPACE_TILE, name, *id)
            })
            .collect();
        tile_list.sort_unstable();
        for (_, name, id) in tile_list {
            tilemap.insert(id.to_string(), text(&name));
        }
        let mut section = Vec::new();
        let mut by_prototype: Vec<_> = by_prototype.into_iter().collect();
        by_prototype.sort_by_cached_key(|(prototype, _)| invariant_order(prototype));
        for (prototype, mut list) in by_prototype {
            list.sort_unstable_by_key(|(uid, _)| *uid);
            let list = list.into_iter().map(|(_, node)| node).collect();
            let mut group = DataMap::new();
            group.insert("proto", text(&prototype));
            group.insert("entities", DataNode::seq(list));
            section.push(DataNode::map(group));
        }
        let mut meta = DataMap::new();
        meta.insert("format", plain(MAP_FORMAT_VERSION));
        meta.insert(
            "category",
            plain(if maps.len() == 1 { "Map" } else { "Unknown" }),
        );
        meta.insert("engineVersion", text("astrocore"));
        meta.insert("forkId", DataNode::string(""));
        meta.insert("forkVersion", DataNode::string(""));
        meta.insert("entityCount", plain(entities.len()));
        let mut root = DataMap::new();
        root.insert("meta", DataNode::map(meta));
        root.insert("maps", DataNode::seq(maps));
        root.insert("grids", DataNode::seq(grids));
        root.insert("orphans", DataNode::seq(Vec::new()));
        root.insert(
            "nullspace",
            DataNode::seq(
                nullspace
                    .iter()
                    .map(|entity| plain(saver.uids[entity]))
                    .collect(),
            ),
        );
        root.insert("tilemap", DataNode::map(tilemap));
        root.insert("entities", DataNode::seq(section));
        Ok(MapSave {
            document: DataNode::map(root),
            entities: entities.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototypes_sort_like_invariant_culture() {
        let mut ids = vec![
            "APCBasic",
            "AcousticGuitarInstrument",
            "Airlock",
            "airlock",
            "Air2",
            "",
        ];
        ids.sort_by_cached_key(|id| invariant_order(id));
        assert_eq!(
            ids,
            [
                "",
                "AcousticGuitarInstrument",
                "Air2",
                "airlock",
                "Airlock",
                "APCBasic"
            ]
        );
    }

    #[test]
    fn base64_round_trips() {
        for bytes in [&[][..], &[1], &[1, 2], &[1, 2, 3], &[0, 255, 7, 9]] {
            let text = encode_base64(bytes);
            assert_eq!(crate::loader::decode_base64(&text).unwrap(), bytes);
        }
        assert_eq!(encode_base64(&[1, 2, 3, 4]), "AQIDBA==");
    }
}
