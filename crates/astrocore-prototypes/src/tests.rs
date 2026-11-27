use astrocore_data::{DataFormat, parse_yaml};
use astrocore_ecs::{
    ComponentAdd, ComponentRegistry, EventWorldExt, LifeStage, MetaData, RegistryWorldExt,
};
use bevy_ecs::component::Component;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Item {
    size: String,
    shape: Vec<String>,
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Tool {
    qualities: Vec<String>,
    speed: f32,
    use_sound: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Reagent {
    id: String,
    name: String,
    #[serde(default)]
    boiling_point: Option<f32>,
    #[serde(default)]
    tags: Vec<String>,
}

impl Prototype for Reagent {
    const KIND: &'static str = "reagent";
    const RULES: FieldRules = FieldRules {
        always: &["tags"],
        never: &[],
    };
}

const YAML: &str = r#"
- type: entity
  abstract: true
  id: BaseItem
  name: item
  description: An item.
  components:
  - type: Item
    size: Small
    shape: [a]
  - type: Sprite
    sprite: base.rsi

- type: entity
  abstract: true
  id: BaseTool
  save: false
  components:
  - type: Tool
    speed: 1
    useSound: tool.ogg
  - type: Item
    size: Large

- type: entity
  parent: [BaseItem, BaseTool]
  id: Crowbar
  name: crowbar
  components:
  - type: Tool
    qualities: [Prying]
  - type: Item

- type: reagent
  id: Base
  abstract: true
  name: base
  boilingPoint: 100
  tags: [liquid]

- type: reagent
  id: Water
  parent: Base
  name: water
  tags: [drink]
  color: blue
"#;

const RON: &str = r#"[
    entity(
        id: "CrowbarRed",
        parent: "Crowbar",
        suffix: "Red",
        components: [
            Tool(speed: 1.5),
            Item(shape: ["b"]),
        ],
    ),
    reagent(id: "Ice", parent: "Water", name: "ice", boilingPoint: None),
]"#;

fn world() -> (World, LoadReport) {
    let mut world = World::new();
    world.register_data_component::<Item>();
    world.register_data_component::<Tool>();
    let mut prototypes = Prototypes::default();
    prototypes.register::<Reagent>();
    let mut report = LoadReport::default();
    prototypes.load_str("items.yml", DataFormat::Yaml, YAML, &mut report);
    prototypes.load_str("red.ron", DataFormat::Ron, RON, &mut report);
    prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
    world.insert_resource(prototypes);
    (world, report)
}

#[test]
fn entity_inheritance_follows_robust_rules() {
    let (world, report) = world();
    assert_eq!(report.errors, []);
    let prototypes = world.resource::<Prototypes>();
    assert!(prototypes.entity("BaseItem").is_none());
    assert!(prototypes.entity_or_abstract("BaseItem").is_some());

    let crowbar = prototypes.entity("Crowbar").unwrap();
    assert_eq!(crowbar.name.as_deref(), Some("crowbar"));
    assert_eq!(crowbar.description.as_deref(), Some("An item."));
    // `abstract` не наследуется, `save` — наследуется.
    assert!(!crowbar.is_abstract && !crowbar.save);
    // Порядок: свои компоненты, затем родительские.
    let names: Vec<_> = crowbar.components.iter().map(|c| &*c.name).collect();
    assert_eq!(names, ["Tool", "Item", "Sprite"]);
    // Первый родитель важнее второго: размер Small из BaseItem.
    let item = crowbar.component("Item").unwrap();
    assert_eq!(item.node.get("size").unwrap().as_str(), Some("Small"));
    let tool = crowbar.component("Tool").unwrap();
    assert_eq!(
        tool.node.get("useSound").unwrap().as_str(),
        Some("tool.ogg")
    );
    assert!(tool.parsed.is_some());

    // Неизменённый компонент потомок делит с родителем.
    let red = prototypes.entity("CrowbarRed").unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &red.component("Sprite").unwrap().node,
        &crowbar.component("Sprite").unwrap().node
    ));
    assert_eq!(red.suffix.as_deref(), Some("Red"));
    // Поле потомка заменяет поле родителя целиком: списки не склеиваются.
    let shape = red.component("Item").unwrap().node.get("shape").unwrap();
    let shape: Vec<_> = shape.as_seq().unwrap().iter().map(|n| n.as_str()).collect();
    assert_eq!(shape, [Some("b")]);
}

#[test]
fn report_lists_what_engine_does_not_know() {
    let (_, report) = world();
    assert_eq!(report.loaded["entity"], 2);
    assert_eq!(report.loaded["reagent"], 2);
    assert_eq!(report.unknown_components["Sprite"], 2);
    assert_eq!(report.unknown_fields["reagent.color"], 2);
    assert!(report.untyped_kinds.is_empty());
}

#[test]
fn typed_prototypes_with_always_push_field() {
    let (world, _) = world();
    let prototypes = world.resource::<Prototypes>();
    assert!(prototypes.get::<Reagent>("Base").is_none());
    let water = prototypes.get::<Reagent>("Water").unwrap();
    assert_eq!(water.boiling_point, Some(100.0));
    assert_eq!(water.tags, ["drink", "liquid"]);
    let ice = prototypes.index(&ProtoId::<Reagent>::new("Ice")).unwrap();
    assert_eq!((ice.name.as_str(), ice.boiling_point), ("ice", None));
    assert_eq!(ice.tags, ["drink", "liquid"]);
    let ids: Vec<_> = prototypes.iter::<Reagent>().map(|(id, _)| id).collect();
    assert_eq!(ids, ["Ice", "Water"]);
    assert!(
        prototypes
            .resolved_node("reagent", "Water")
            .unwrap()
            .get("type")
            .is_none()
    );
}

#[derive(Resource, Default)]
struct Added(Vec<&'static str>);

#[test]
fn spawns_entity_with_components_and_overrides() {
    let (mut world, _) = world();
    world.init_resource::<Added>();
    world.subscribe::<Item, ComponentAdd>(|w, _, _| w.resource_mut::<Added>().0.push("Item"));
    world.subscribe::<Tool, ComponentAdd>(|w, _, _| w.resource_mut::<Added>().0.push("Tool"));

    let crowbar = world.spawn_prototype("CrowbarRed").unwrap();
    let meta = world.get::<MetaData>(crowbar).unwrap();
    assert_eq!(meta.prototype.as_deref(), Some("CrowbarRed"));
    assert_eq!(meta.stage, LifeStage::Spawned);
    assert_eq!(
        world.get::<Tool>(crowbar),
        Some(&Tool {
            qualities: vec!["Prying".into()],
            speed: 1.5,
            use_sound: Some("tool.ogg".into()),
        })
    );
    assert_eq!(world.get::<Item>(crowbar).unwrap().shape, ["b"]);
    assert_eq!(world.resource::<Added>().0, ["Tool", "Item"]);

    let speed = parse_yaml("speed: 3").unwrap();
    let overrides = EntityOverrides {
        components: vec![("Tool", &speed)],
        missing: vec!["Item"],
    };
    let fast = world.spawn_prototype_with("Crowbar", &overrides).unwrap();
    let tool = world.get::<Tool>(fast).unwrap();
    assert_eq!((tool.speed, tool.qualities.len()), (3.0, 1));
    assert!(world.get::<Item>(fast).is_none());

    assert_eq!(
        world.spawn_prototype("BaseItem"),
        Err(SpawnError::UnknownPrototype("BaseItem".into()))
    );
    let bad = parse_yaml("speed: fast").unwrap();
    let before = world.entities().len();
    let overrides = EntityOverrides {
        components: vec![("Tool", &bad)],
        missing: vec![],
    };
    assert!(matches!(
        world.spawn_prototype_with("Crowbar", &overrides),
        Err(SpawnError::BadOverride { .. })
    ));
    assert_eq!(world.entities().len(), before);
}

#[test]
fn errors_do_not_stop_loading() {
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str(
        "bad.yml",
        DataFormat::Yaml,
        r#"
- type: entity
  id: A
  parent: Missing
- type: entity
  id: B
  parent: C
- type: entity
  id: C
  parent: B
- type: entity
  id: Good
- type: entity
  id: Good
- id: NoKind
- type: entity
"#,
        &mut report,
    );
    prototypes.load_str("broken.yml", DataFormat::Yaml, "- [unclosed", &mut report);
    prototypes.resolve(&ComponentRegistry::default(), &mut report);
    assert!(prototypes.entity("Good").is_some());
    assert!(prototypes.entity("A").is_none() && prototypes.entity("B").is_none());
    let messages: Vec<_> = report.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), 6, "{messages:#?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("неизвестный родитель Missing"))
    );
    assert!(messages.iter().any(|m| m.contains("цикл наследования")));
    assert!(messages.iter().any(|m| m.contains("объявлен дважды")));
    assert!(
        messages.iter().any(|m| m.starts_with("bad.yml:13:")),
        "{messages:#?}"
    );
}

#[test]
fn loads_directory_with_both_formats() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("items.yml"), YAML).unwrap();
    std::fs::write(dir.path().join("sub/red.ron"), RON).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "не прототип").unwrap();
    let mut world = World::new();
    world.register_data_component::<Item>();
    world.register_data_component::<Tool>();
    let report = world.load_prototypes(dir.path());
    assert_eq!(report.errors, []);
    assert_eq!(report.files, 2);
    assert!(report.untyped_kinds.contains("reagent"));
    assert!(world.spawn_prototype("CrowbarRed").is_ok());
}

#[test]
fn create_variants_expands_one_record_into_several() {
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str(
        "pumps.yml",
        DataFormat::Yaml,
        r#"
- type: entity
  id: !type:CreateVariants
    values: [ Pump, PumpAlt1, PumpAlt2 ]
  name: pump
  categories: !type:CreateVariants
    sequences: [ [ ], [ HideSpawnMenu ], [ HideSpawnMenu ] ]
  components:
  - type: Sprite
    layers:
    - sprite: !type:CreateVariants
        values: [ pump.rsi, pump_alt1.rsi, pump_alt2.rsi ]
- type: entity
  id: !type:CreateVariants
    values: [ A, B ]
  name: !type:CreateVariants
    values: [ only one ]
"#,
        &mut report,
    );
    prototypes.resolve(&ComponentRegistry::default(), &mut report);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0]
            .message
            .contains("нет значения для варианта 1")
    );

    let alt = prototypes.entity("PumpAlt1").unwrap();
    assert_eq!(alt.categories, ["HideSpawnMenu"]);
    assert!(prototypes.entity("Pump").unwrap().categories.is_empty());
    let sprite = &alt.component("Sprite").unwrap().node;
    let layer = &sprite.get("layers").unwrap().as_seq().unwrap()[0];
    assert_eq!(layer.get("sprite").unwrap().as_str(), Some("pump_alt1.rsi"));
    let collection: Vec<_> = prototypes
        .variants(ENTITY_KIND, "PumpAlt2")
        .unwrap()
        .iter()
        .map(|id| &**id)
        .collect();
    assert_eq!(collection, ["Pump", "PumpAlt1", "PumpAlt2"]);
    assert!(prototypes.variants(ENTITY_KIND, "Missing").is_none());
}

#[test]
fn parents_of_any_kind_are_kept() {
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    prototypes.load_str(
        "groups.yml",
        DataFormat::Yaml,
        r#"
- type: markingsGroup
  id: Base
  limits: { Top: 1 }
- type: markingsGroup
  id: Child
  parent: Base
  limits: { Hair: 2 }
- type: markingsGroup
  id: Both
  parent: [ Base, Child ]
"#,
        &mut report,
    );
    prototypes.resolve(&ComponentRegistry::default(), &mut report);
    assert_eq!(prototypes.parents("markingsGroup", "Child"), ["Base"]);
    assert_eq!(
        prototypes.parents("markingsGroup", "Both"),
        ["Base", "Child"]
    );
    assert!(prototypes.parents("markingsGroup", "Base").is_empty());
    assert!(prototypes.parents("markingsGroup", "Missing").is_empty());
}
