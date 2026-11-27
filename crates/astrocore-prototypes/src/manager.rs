//! Менеджер прототипов. Образец — `Robust.Shared/Prototypes/PrototypeManager*.cs` @ 1afd692.
//!
//! Файл прототипов — список записей. Вид записи задаёт ключ `type` (YAML SS14) или имя
//! структуры (RON): `- type: entity` и `entity(...)` равнозначны. Загрузка идёт в два шага:
//! [`Prototypes::load_dir`] читает файлы, [`Prototypes::resolve`] применяет наследование и
//! разбирает записи в типы Rust.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use astrocore_data::{DataError, DataFormat, DataMap, DataNode, Value, from_node_checked};
use astrocore_ecs::{ComponentRegistry, ErasedComponent};
use bevy_ecs::component::ComponentId;
use bevy_ecs::resource::Resource;
use rayon::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::inherit::{FieldRules, OrderError, merge_maps, merge_nodes, resolution_order};

/// Вид прототипов сущностей.
pub const ENTITY_KIND: &str = "entity";

/// Ключи записи, которые не являются полями прототипа.
const SERVICE_KEYS: [&str; 3] = ["type", "parent", "abstract"];

/// Поля прототипа сущности, которые ядро читает само.
const ENTITY_FIELDS: [&str; 13] = [
    "type",
    "id",
    "parent",
    "abstract",
    "name",
    "description",
    "suffix",
    "categories",
    "save",
    "placement",
    "localizationId",
    "components",
    "loc",
];

const ENTITY_RULES: FieldRules = FieldRules {
    always: &[],
    never: &["abstract", "categories"],
};

/// Прототип с типом Rust: запись вида `KIND`.
pub trait Prototype: DeserializeOwned + Send + Sync + 'static {
    /// Значение ключа `type` в данных.
    const KIND: &'static str;
    /// Правила наследования полей.
    const RULES: FieldRules = FieldRules {
        always: &[],
        never: &[],
    };
}

/// Идентификатор прототипа вида `T` (`ProtoId<T>` в Robust).
#[derive(Serialize, Deserialize)]
#[serde(transparent, bound = "")]
pub struct ProtoId<T> {
    id: Arc<str>,
    #[serde(skip)]
    kind: PhantomData<fn() -> T>,
}

impl<T> ProtoId<T> {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self {
            id: id.into(),
            kind: PhantomData,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

impl<T> Clone for ProtoId<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            kind: PhantomData,
        }
    }
}

impl<T> PartialEq for ProtoId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for ProtoId<T> {}

impl<T> std::hash::Hash for ProtoId<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> std::fmt::Debug for ProtoId<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.id)
    }
}

impl<T> std::fmt::Display for ProtoId<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id)
    }
}

/// Идентификатор прототипа сущности (`EntProtoId` в Robust).
pub type EntProtoId = ProtoId<EntityPrototype>;

/// Компонент прототипа сущности после наследования.
#[derive(Clone)]
pub struct PrototypeComponent {
    /// Имя компонента в данных.
    pub name: Arc<str>,
    /// Поля компонента. Узел общий с родителем, если потомок компонент не менял.
    pub node: Arc<DataNode>,
    /// Разобранный компонент; `None` — тип не зарегистрирован в движке.
    pub parsed: Option<Arc<dyn ErasedComponent>>,
    pub component_id: Option<ComponentId>,
}

/// Прототип сущности (`EntityPrototype` в Robust).
pub struct EntityPrototype {
    pub id: Arc<str>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub suffix: Option<String>,
    pub parents: Vec<String>,
    pub is_abstract: bool,
    /// Категории, заданные самим прототипом.
    pub categories: Vec<String>,
    /// Сохранять ли сущности этого прототипа в файл карты.
    pub save: bool,
    pub components: Vec<PrototypeComponent>,
    /// Все поля верхнего уровня после наследования, кроме `components`.
    pub data: DataNode,
}

impl EntityPrototype {
    pub fn component(&self, name: &str) -> Option<&PrototypeComponent> {
        self.components.iter().find(|c| &*c.name == name)
    }
}

/// Ошибка в данных прототипов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrototypeError {
    /// Файл или иной источник данных.
    pub source: String,
    pub line: u32,
    pub message: String,
}

impl std::fmt::Display for PrototypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            0 => write!(f, "{}: {}", self.source, self.message),
            line => write!(f, "{}:{line}: {}", self.source, self.message),
        }
    }
}

/// Итог загрузки: ошибки и то, что движок пока не понимает.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Прочитано файлов.
    pub files: usize,
    pub errors: Vec<PrototypeError>,
    /// Вид → число записей (без абстрактных).
    pub loaded: BTreeMap<String, usize>,
    /// Виды без типа Rust: записи доступны только как дерево данных.
    pub untyped_kinds: BTreeSet<String>,
    /// Незарегистрированный компонент → число прототипов сущностей с ним.
    pub unknown_components: BTreeMap<String, usize>,
    /// `Вид.поле` или `Компонент.поле` → число записей с полем, которого нет у типа Rust.
    pub unknown_fields: BTreeMap<String, usize>,
}

impl LoadReport {
    fn error(&mut self, source: &str, line: u32, message: impl Into<String>) {
        self.errors.push(PrototypeError {
            source: source.to_owned(),
            line,
            message: message.into(),
        });
    }
}

struct RawEntry {
    parents: Vec<String>,
    is_abstract: bool,
    node: DataNode,
    source: Arc<str>,
}

type TypedParse = fn(&DataNode) -> Result<(Arc<dyn Any + Send + Sync>, Vec<String>), DataError>;

#[derive(Default)]
struct Kind {
    raw: BTreeMap<Arc<str>, RawEntry>,
    /// Записи после наследования, включая абстрактные.
    resolved: HashMap<Arc<str>, Arc<DataNode>>,
    typed: BTreeMap<Arc<str>, Arc<dyn Any + Send + Sync>>,
    parse: Option<TypedParse>,
    rules: FieldRules,
    /// Прототип → все варианты его набора.
    variants: HashMap<Arc<str>, Arc<[Arc<str>]>>,
}

/// Все прототипы игры. Ресурс `World`.
#[derive(Resource, Default)]
pub struct Prototypes {
    kinds: BTreeMap<Arc<str>, Kind>,
    entities: BTreeMap<Arc<str>, Arc<EntityPrototype>>,
}

fn parse_typed<T: Prototype>(
    node: &DataNode,
) -> Result<(Arc<dyn Any + Send + Sync>, Vec<String>), DataError> {
    let (value, unknown) = from_node_checked::<T>(node)?;
    let unknown = unknown.into_iter().map(|field| field.name).collect();
    Ok((Arc::new(value), unknown))
}

/// Список родителей: `parent: Base` или `parent: [A, B]`.
fn parents_of(node: &DataNode) -> Result<Vec<String>, String> {
    let Some(parent) = node.get("parent") else {
        return Ok(Vec::new());
    };
    match &parent.value {
        _ if parent.is_null() => Ok(Vec::new()),
        Value::Scalar(scalar) => Ok(vec![scalar.text.clone()]),
        Value::Seq(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "родитель должен быть строкой".to_owned())
            })
            .collect(),
        Value::Map(_) => Err("поле parent должно быть строкой или списком строк".to_owned()),
    }
}

fn flag(node: &DataNode, key: &str, default: bool) -> bool {
    match node.get(key).and_then(DataNode::as_str) {
        Some("true" | "True") => true,
        Some("false" | "False") => false,
        _ => default,
    }
}

fn text(node: &DataNode, key: &str) -> Option<String> {
    node.get(key).and_then(DataNode::as_str).map(str::to_owned)
}

/// Узел без служебных ключей и тега вида: то, что разбирается в тип Rust.
fn strip_service(node: &DataNode, keys: &[&str]) -> DataNode {
    let mut copy = node.clone();
    copy.tag = None;
    if let Some(map) = copy.as_map_mut() {
        map.0.retain(|(key, _)| !keys.contains(&key.as_str()));
    }
    copy
}

/// Тег узла, который задаёт значения поля для каждого варианта прототипа.
const VARIANTS_TAG: &str = "type:CreateVariants";

fn is_variants(node: &DataNode) -> bool {
    node.tag.as_deref() == Some(VARIANTS_TAG) && node.as_map().is_some()
}

/// Заменяет узлы `!type:CreateVariants` значением варианта с номером `index`:
/// элементом `values` или списком из `sequences`.
/// Образец — `PrototypeManager.Variantization.cs` Robust @ 1afd692.
fn select_variant(node: &mut DataNode, index: usize) -> Result<(), String> {
    match &mut node.value {
        Value::Scalar(_) => Ok(()),
        Value::Seq(items) => items
            .iter_mut()
            .try_for_each(|item| select_variant(item, index)),
        Value::Map(map) => {
            for (key, child) in &mut map.0 {
                if !is_variants(child) {
                    select_variant(child, index)?;
                    continue;
                }
                let line = child.line;
                let chosen = match (child.get("sequences"), child.get("values")) {
                    (Some(sequences), _) => sequences.as_seq().and_then(|s| s.get(index)).cloned(),
                    (None, Some(values)) => values.as_seq().and_then(|s| s.get(index)).cloned(),
                    (None, None) => None,
                };
                let Some(mut chosen) = chosen else {
                    return Err(format!(
                        "поле {key}: в CreateVariants нет значения для варианта {index}"
                    ));
                };
                chosen.line = line;
                *child = chosen;
            }
            Ok(())
        }
    }
}

/// Разворачивает запись с `id: !type:CreateVariants` в записи вариантов.
fn expand_variants(item: &DataNode) -> Result<Vec<DataNode>, String> {
    let count = item
        .get("id")
        .and_then(|id| id.get("values"))
        .and_then(DataNode::as_seq)
        .map(<[DataNode]>::len)
        .filter(|&count| count > 0)
        .ok_or("в `id: !type:CreateVariants` нужен непустой список values")?;
    (0..count)
        .map(|index| {
            let mut variant = item.clone();
            select_variant(&mut variant, index)?;
            Ok(variant)
        })
        .collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if DataFormat::from_path(&path).is_some() {
            out.push(path);
        }
    }
    Ok(())
}

impl Prototypes {
    /// Регистрирует вид прототипов с типом Rust.
    pub fn register<T: Prototype>(&mut self) {
        let kind = self.kinds.entry(Arc::from(T::KIND)).or_default();
        kind.parse = Some(parse_typed::<T>);
        kind.rules = T::RULES;
    }

    /// Читает все файлы `.yml`, `.yaml` и `.ron` каталога и его подкаталогов.
    pub fn load_dir(&mut self, dir: &Path, report: &mut LoadReport) {
        let mut files = Vec::new();
        if let Err(error) = collect_files(dir, &mut files) {
            report.error(&dir.display().to_string(), 0, error.to_string());
            return;
        }
        files.sort();
        let parsed: Vec<(String, Result<DataNode, DataError>)> = files
            .par_iter()
            .map(|path| {
                let source = path
                    .strip_prefix(dir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let node = std::fs::read_to_string(path)
                    .map_err(|e| DataError::new(e.to_string(), 0))
                    .and_then(|text| match DataFormat::from_path(path) {
                        Some(format) => format.parse(&text),
                        None => Ok(DataNode::null()),
                    });
                (source, node)
            })
            .collect();
        for (source, node) in parsed {
            match node {
                Ok(node) => self.load_node(&source, &node, report),
                Err(error) => report.error(&source, error.line, error.message),
            }
        }
    }

    /// Читает прототипы из текста.
    pub fn load_str(
        &mut self,
        source: &str,
        format: DataFormat,
        text: &str,
        report: &mut LoadReport,
    ) {
        match format.parse(text) {
            Ok(node) => self.load_node(source, &node, report),
            Err(error) => report.error(source, error.line, error.message),
        }
    }

    /// Читает прототипы из разобранного документа: списка записей.
    pub fn load_node(&mut self, source: &str, root: &DataNode, report: &mut LoadReport) {
        report.files += 1;
        if root.is_null() {
            return;
        }
        let Some(items) = root.as_seq() else {
            report.error(source, root.line, "ожидался список прототипов");
            return;
        };
        let source: Arc<str> = Arc::from(source);
        for item in items {
            let Some(kind) = item.type_name() else {
                report.error(&source, item.line, "у записи нет вида (`type`)");
                continue;
            };
            // Запись с `id: !type:CreateVariants` порождает несколько прототипов.
            if item.get("id").is_some_and(is_variants) {
                match expand_variants(item) {
                    Ok(variants) => {
                        let ids: Arc<[Arc<str>]> = variants
                            .iter()
                            .filter_map(|v| v.get("id").and_then(DataNode::as_str).map(Arc::from))
                            .collect();
                        let kind = Arc::<str>::from(kind);
                        for variant in &variants {
                            self.load_record(&source, &kind, variant, report);
                        }
                        let collections = &mut self.kinds.entry(kind).or_default().variants;
                        for id in ids.iter() {
                            collections.insert(id.clone(), ids.clone());
                        }
                    }
                    Err(message) => {
                        report.error(&source, item.line, format!("{kind}: {message}"));
                    }
                }
                continue;
            }
            let kind = Arc::<str>::from(kind);
            self.load_record(&source, &kind, item, report);
        }
    }

    fn load_record(
        &mut self,
        source: &Arc<str>,
        kind: &Arc<str>,
        item: &DataNode,
        report: &mut LoadReport,
    ) {
        {
            let Some(id) = item.get("id").and_then(DataNode::as_str) else {
                report.error(
                    source,
                    item.line,
                    format!("у записи вида {kind} нет идентификатора (`id`)"),
                );
                return;
            };
            let parents = match parents_of(item) {
                Ok(parents) => parents,
                Err(message) => {
                    report.error(source, item.line, format!("{kind} {id}: {message}"));
                    return;
                }
            };
            let entry = RawEntry {
                parents,
                is_abstract: flag(item, "abstract", false),
                node: item.clone(),
                source: source.clone(),
            };
            let kind_name = kind;
            let kind = self.kinds.entry(kind.clone()).or_default();
            if let Some(previous) = kind.raw.get(id) {
                report.error(
                    source,
                    item.line,
                    format!(
                        "{kind_name} {id} объявлен дважды (первый раз в {})",
                        previous.source
                    ),
                );
                return;
            }
            kind.raw.insert(Arc::from(id), entry);
        }
    }

    /// Все варианты набора, в который входит прототип (`TryGetVariantCollection` в Robust):
    /// прототипы, порождённые одной записью с `!type:CreateVariants`.
    pub fn variants(&self, kind: &str, id: &str) -> Option<&[Arc<str>]> {
        self.kinds.get(kind)?.variants.get(id).map(Arc::as_ref)
    }

    /// Применяет наследование и разбирает записи. Вызывается после загрузки всех файлов;
    /// повторный вызов пересчитывает всё заново.
    pub fn resolve(&mut self, registry: &ComponentRegistry, report: &mut LoadReport) {
        let kinds: Vec<Arc<str>> = self.kinds.keys().cloned().collect();
        for name in kinds {
            if &*name == ENTITY_KIND {
                self.resolve_entities(registry, report);
            } else if let Some(kind) = self.kinds.get_mut(&name) {
                resolve_kind(&name, kind, report);
            }
        }
    }

    fn resolve_entities(&mut self, registry: &ComponentRegistry, report: &mut LoadReport) {
        struct Resolved {
            top: DataMap,
            components: Vec<(Arc<str>, Arc<DataNode>)>,
        }

        self.entities.clear();
        let Some(kind) = self.kinds.get(ENTITY_KIND) else {
            return;
        };
        let parents: BTreeMap<&str, &[String]> = kind
            .raw
            .iter()
            .map(|(id, entry)| (&**id, entry.parents.as_slice()))
            .collect();
        let (order, errors) = resolution_order(&parents);
        report_order_errors(ENTITY_KIND, &kind.raw, errors, report);

        let mut resolved: HashMap<&str, Resolved> = HashMap::with_capacity(order.len());
        for id in &order {
            let (id, entry) = kind.raw.get_key_value(*id).expect("запись из порядка");
            let mut top = entry.node.as_map().cloned().unwrap_or_default();
            let own_components = top.remove("components");
            let mut components: Vec<(Arc<str>, Arc<DataNode>)> = Vec::new();
            for item in own_components
                .as_ref()
                .and_then(DataNode::as_seq)
                .unwrap_or_default()
            {
                let Some(name) = item.type_name() else {
                    report.error(
                        &entry.source,
                        item.line,
                        format!("entity {id}: у компонента нет типа (`type`)"),
                    );
                    continue;
                };
                if components.iter().any(|(existing, _)| &**existing == name) {
                    report.error(
                        &entry.source,
                        item.line,
                        format!("entity {id}: компонент {name} задан дважды"),
                    );
                    continue;
                }
                components.push((Arc::from(name), Arc::new(strip_service(item, &["type"]))));
            }
            // Родители применяются по порядку: первый родитель важнее следующих.
            for parent in &entry.parents {
                let parent = &resolved[parent.as_str()];
                top = merge_maps(&top, &parent.top, ENTITY_RULES);
                for (name, inherited) in &parent.components {
                    match components.iter_mut().find(|(own, _)| own == name) {
                        Some((_, own)) if Arc::ptr_eq(own, inherited) => {}
                        Some((_, own)) => {
                            let rules = registry.get(name).map_or(FieldRules::default(), |info| {
                                FieldRules {
                                    always: info.inherit_always,
                                    never: info.inherit_never,
                                }
                            });
                            *own = Arc::new(merge_nodes(own, inherited, rules));
                        }
                        None => components.push((name.clone(), inherited.clone())),
                    }
                }
            }
            resolved.insert(id, Resolved { top, components });
        }

        // Один узел данных разбирается один раз, сколько бы прототипов его ни наследовало.
        let mut parsed: HashMap<*const DataNode, Option<Arc<dyn ErasedComponent>>> = HashMap::new();
        let mut loaded = 0;
        for id in order {
            let (id, entry) = kind.raw.get_key_value(id).expect("запись из порядка");
            let Some(Resolved { top, components }) = resolved.remove(&**id) else {
                continue;
            };
            let data = DataNode::map(top).with_line(entry.node.line);
            let mut prototype = EntityPrototype {
                id: id.clone(),
                name: text(&data, "name"),
                description: text(&data, "description"),
                suffix: text(&data, "suffix"),
                parents: entry.parents.clone(),
                is_abstract: entry.is_abstract,
                categories: entry
                    .node
                    .get("categories")
                    .and_then(DataNode::as_seq)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect(),
                save: flag(&data, "save", true),
                components: Vec::with_capacity(components.len()),
                data,
            };
            for (name, node) in components {
                let info = registry.get(&name);
                let component = match info {
                    // Абстрактные прототипы не создаются: их компоненты разбирать незачем.
                    Some(_) if entry.is_abstract => None,
                    Some(info) => parsed
                        .entry(Arc::as_ptr(&node))
                        .or_insert_with(|| match info.parse(&node) {
                            Ok(result) => {
                                for field in result.unknown {
                                    *report
                                        .unknown_fields
                                        .entry(format!("{name}.{}", field.name))
                                        .or_default() += 1;
                                }
                                Some(Arc::from(result.component))
                            }
                            Err(error) => {
                                report.error(
                                    &entry.source,
                                    error.line,
                                    format!("entity {id}, компонент {name}: {}", error.message),
                                );
                                None
                            }
                        })
                        .clone(),
                    None => {
                        if !entry.is_abstract {
                            *report
                                .unknown_components
                                .entry(name.to_string())
                                .or_default() += 1;
                        }
                        None
                    }
                };
                prototype.components.push(PrototypeComponent {
                    name,
                    node,
                    parsed: component,
                    component_id: info.map(|i| i.id),
                });
            }
            if !entry.is_abstract {
                loaded += 1;
                for key in prototype.data.as_map().into_iter().flat_map(DataMap::keys) {
                    if !ENTITY_FIELDS.contains(&key) {
                        *report
                            .unknown_fields
                            .entry(format!("{ENTITY_KIND}.{key}"))
                            .or_default() += 1;
                    }
                }
            }
            self.entities.insert(id.clone(), Arc::new(prototype));
        }
        report.loaded.insert(ENTITY_KIND.to_owned(), loaded);
    }

    /// Прототип вида `T`. Абстрактные прототипы не возвращаются.
    pub fn get<T: Prototype>(&self, id: &str) -> Option<&T> {
        self.kinds.get(T::KIND)?.typed.get(id)?.downcast_ref()
    }

    /// Прототип по типизированному идентификатору.
    pub fn index<T: Prototype>(&self, id: &ProtoId<T>) -> Option<&T> {
        self.get(id.as_str())
    }

    /// Все прототипы вида `T` по порядку идентификаторов.
    pub fn iter<T: Prototype>(&self) -> impl Iterator<Item = (&str, &T)> {
        self.kinds.get(T::KIND).into_iter().flat_map(|kind| {
            kind.typed
                .iter()
                .filter_map(|(id, value)| Some((&**id, value.downcast_ref()?)))
        })
    }

    /// Прототип сущности, из которого можно создать сущность (не абстрактный).
    pub fn entity(&self, id: &str) -> Option<&Arc<EntityPrototype>> {
        self.entities.get(id).filter(|proto| !proto.is_abstract)
    }

    /// Прототип сущности, включая абстрактные.
    pub fn entity_or_abstract(&self, id: &str) -> Option<&Arc<EntityPrototype>> {
        self.entities.get(id)
    }

    /// Все не абстрактные прототипы сущностей по порядку идентификаторов.
    pub fn entities(&self) -> impl Iterator<Item = &Arc<EntityPrototype>> {
        self.entities.values().filter(|proto| !proto.is_abstract)
    }

    /// Запись любого вида после наследования — дерево данных без служебных ключей.
    pub fn resolved_node(&self, kind: &str, id: &str) -> Option<&DataNode> {
        self.kinds.get(kind)?.resolved.get(id).map(Arc::as_ref)
    }

    /// Родители записи, как они заданы в данных (`Parents` в Robust). Для полей, которые
    /// складываются с родительскими (`[AlwaysPushInheritance]`) вне прототипов сущностей.
    pub fn parents(&self, kind: &str, id: &str) -> &[String] {
        self.kinds
            .get(kind)
            .and_then(|k| k.raw.get(id))
            .map_or(&[], |entry| entry.parents.as_slice())
    }

    /// Идентификаторы записей вида, включая абстрактные.
    pub fn ids(&self, kind: &str) -> impl Iterator<Item = &str> {
        self.kinds
            .get(kind)
            .into_iter()
            .flat_map(|k| k.raw.keys().map(|id| &**id))
    }

    /// Загруженные виды прототипов.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.kinds.keys().map(|k| &**k)
    }
}

fn report_order_errors(
    kind_name: &str,
    raw: &BTreeMap<Arc<str>, RawEntry>,
    errors: Vec<OrderError>,
    report: &mut LoadReport,
) {
    for error in errors {
        let (id, message) = match &error {
            OrderError::UnknownParent { child, parent } => {
                (child.as_str(), format!("неизвестный родитель {parent}"))
            }
            OrderError::Cycle(cycle) => (
                cycle[0].as_str(),
                format!("цикл наследования: {}", cycle.join(" → ")),
            ),
            OrderError::TooDeep(id) => (id.as_str(), "слишком глубокое наследование".to_owned()),
        };
        let (source, line) = raw
            .get(id)
            .map_or(("", 0), |entry| (&*entry.source, entry.node.line));
        report.error(source, line, format!("{kind_name} {id}: {message}"));
    }
}

fn resolve_kind(name: &str, kind: &mut Kind, report: &mut LoadReport) {
    kind.resolved.clear();
    kind.typed.clear();
    let parents: BTreeMap<&str, &[String]> = kind
        .raw
        .iter()
        .map(|(id, entry)| (&**id, entry.parents.as_slice()))
        .collect();
    let (order, errors) = resolution_order(&parents);
    report_order_errors(name, &kind.raw, errors, report);

    let mut loaded = 0;
    for id in order {
        let (id, entry) = kind.raw.get_key_value(id).expect("запись из порядка");
        let mut node = entry.node.clone();
        for parent in &entry.parents {
            node = merge_nodes(&node, &kind.resolved[parent.as_str()], kind.rules);
        }
        // Служебные ключи потомка остаются до конца слияния: `abstract` не наследуется.
        let mut inheritable = node.clone();
        if let Some(map) = inheritable.as_map_mut() {
            map.0.retain(|(key, _)| key != "abstract");
        }
        kind.resolved.insert(id.clone(), Arc::new(inheritable));
        if entry.is_abstract {
            continue;
        }
        loaded += 1;
        let Some(parse) = kind.parse else {
            continue;
        };
        match parse(&strip_service(&node, &SERVICE_KEYS)) {
            Ok((value, unknown)) => {
                for field in unknown {
                    *report
                        .unknown_fields
                        .entry(format!("{name}.{field}"))
                        .or_default() += 1;
                }
                kind.typed.insert(id.clone(), value);
            }
            Err(error) => report.error(
                &entry.source,
                error.line,
                format!("{name} {id}: {}", error.message),
            ),
        }
    }
    // Разрешённые записи хранятся без служебных ключей.
    for node in kind.resolved.values_mut() {
        *node = Arc::new(strip_service(node, &SERVICE_KEYS));
    }
    report.loaded.insert(name.to_owned(), loaded);
    if kind.parse.is_none() {
        report.untyped_kinds.insert(name.to_owned());
    }
}
