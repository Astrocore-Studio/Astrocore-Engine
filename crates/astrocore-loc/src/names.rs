//! Названия и описания сущностей из перевода (`Loc.GetEntityData` Robust @ 1afd692).
//!
//! Название прототипа — сообщение `ent-<идентификатор>`, описание — его атрибут `.desc`.
//! Чего у прототипа нет, берётся у его родителей. Названия считаются один раз после
//! загрузки прототипов и языка: сеть их не передаёт, каждый клиент видит свой язык.

use std::collections::{HashMap, HashSet};

use astrocore_ecs::MetaData;
use astrocore_prototypes::Prototypes;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use crate::Loc;

/// Название и описание прототипа сущности.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityText {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Грамматические признаки из атрибутов сообщения (`.gender`, `.proper`…), с родителей.
    pub attributes: std::collections::BTreeMap<String, String>,
}

/// Названия прототипов сущностей на языке этого мира. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct EntityNames(HashMap<String, EntityText>);

impl EntityNames {
    pub fn get(&self, prototype: &str) -> Option<&EntityText> {
        self.0.get(prototype)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Текст прототипа: перевод, затем поля `name` и `description` прототипа, затем родители
/// по порядку.
fn resolve(
    loc: &Loc,
    prototypes: &Prototypes,
    id: &str,
    done: &mut HashMap<String, EntityText>,
    walking: &mut HashSet<String>,
) -> EntityText {
    if let Some(text) = done.get(id) {
        return text.clone();
    }
    let Some(prototype) = prototypes.entity_or_abstract(id) else {
        return EntityText::default();
    };
    // Цикл в родителях отсекает загрузка прототипов; здесь он только не должен зациклить.
    if !walking.insert(id.to_owned()) {
        return EntityText::default();
    }
    let mut text = EntityText {
        name: loc.entity_name(id).or_else(|| prototype.name.clone()),
        description: loc
            .entity_description(id)
            .or_else(|| prototype.description.clone()),
        attributes: loc.entity_attributes(id),
    };
    for parent in &prototype.parents {
        let inherited = resolve(loc, prototypes, parent, done, walking);
        text.name = text.name.or(inherited.name);
        text.description = text.description.or(inherited.description);
        for (key, value) in inherited.attributes {
            text.attributes.entry(key).or_insert(value);
        }
    }
    walking.remove(id);
    done.insert(id.to_owned(), text.clone());
    text
}

/// Названия сущностей на `World`.
pub trait EntityNameWorldExt {
    /// Считает названия прототипов по загруженным прототипам и языку. Вызывается после
    /// их загрузки и после смены языка. Возвращает число прототипов с названием.
    fn build_entity_names(&mut self) -> usize;

    /// Название сущности: своё имя, иначе название прототипа, иначе его идентификатор.
    fn entity_name(&self, entity: Entity) -> String;

    /// Описание сущности: своё, иначе описание прототипа.
    fn entity_description(&self, entity: Entity) -> Option<String>;

    /// Сущность как аргумент сообщения: название и признаки прототипа, поверх них —
    /// компонента `Grammar` (`TryGetEntityLocAttrib`).
    fn entity_arg(&self, entity: Entity) -> crate::EntityArg;
}

impl EntityNameWorldExt for World {
    fn build_entity_names(&mut self) -> usize {
        let fallback = Loc::default();
        let mut done = HashMap::new();
        if let Some(prototypes) = self.get_resource::<Prototypes>() {
            let loc = self.get_resource::<Loc>().unwrap_or(&fallback);
            let ids: Vec<String> = prototypes.ids("entity").map(str::to_owned).collect();
            for id in ids {
                resolve(loc, prototypes, &id, &mut done, &mut HashSet::new());
            }
        }
        let named = done.values().filter(|text| text.name.is_some()).count();
        self.insert_resource(EntityNames(done));
        named
    }

    fn entity_name(&self, entity: Entity) -> String {
        let Some(meta) = self.get::<MetaData>(entity) else {
            return format!("{entity}");
        };
        if let Some(name) = &meta.name {
            return name.clone();
        }
        let Some(prototype) = meta.prototype.as_deref() else {
            return format!("{entity}");
        };
        self.get_resource::<EntityNames>()
            .and_then(|names| names.get(prototype)?.name.clone())
            .unwrap_or_else(|| prototype.to_owned())
    }

    fn entity_arg(&self, entity: Entity) -> crate::EntityArg {
        let mut arg = crate::EntityArg::new(self.entity_name(entity));
        if let Some(prototype) = self
            .get::<MetaData>(entity)
            .and_then(|m| m.prototype.as_deref())
            && let Some(text) = self
                .get_resource::<EntityNames>()
                .and_then(|n| n.get(prototype))
        {
            arg.attributes = text.attributes.clone();
        }
        if let Some(grammar) = self.get::<crate::Grammar>(entity) {
            arg.attributes.extend(
                grammar
                    .attributes
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        arg
    }

    fn entity_description(&self, entity: Entity) -> Option<String> {
        let meta = self.get::<MetaData>(entity)?;
        if let Some(description) = &meta.description {
            return Some(description.clone());
        }
        self.get_resource::<EntityNames>()?
            .get(meta.prototype.as_deref()?)?
            .description
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use astrocore_data::DataFormat;
    use astrocore_ecs::{ComponentRegistry, GameWorldExt};
    use astrocore_prototypes::{LoadReport, PrototypeWorldExt};
    use astrocore_resources::Resources;

    use super::*;

    const PROTOTYPES: &str = "
- type: entity
  id: BaseWall
  abstract: true
- type: entity
  id: WallSolid
  parent: BaseWall
- type: entity
  id: WallSolidRust
  parent: WallSolid
- type: entity
  id: Mystery
- type: entity
  id: OldStyle
  name: старое имя
  description: описание из прототипа
";

    const LOCALE: &str = "ent-BaseWall = преграда
    .desc = Не пройти.
    .gender = female
ent-WallSolid = сплошная стена
ent-WallSolidRust = { ent-WallSolid }
    .desc = Ржавая.
";

    #[test]
    fn names_come_from_locale_prototype_and_parents() {
        let dir = tempfile::tempdir().unwrap();
        let locale = dir.path().join("Locale/ru-RU");
        std::fs::create_dir_all(&locale).unwrap();
        std::fs::write(locale.join("walls.ftl"), LOCALE).unwrap();
        let mut resources = Resources::default();
        resources.mount(dir.path());

        let mut world = World::new();
        world.init_game();
        let mut prototypes = Prototypes::default();
        let mut report = LoadReport::default();
        prototypes.load_str("t.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
        prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
        assert_eq!(report.errors, []);
        world.insert_resource(prototypes);
        let mut loc = Loc::default();
        assert_eq!(
            loc.load_culture(&resources, "ru-RU").unwrap().errors,
            Vec::<String>::new()
        );
        world.insert_resource(loc);
        assert_eq!(world.build_entity_names(), 4);

        let wall = world.spawn_prototype("WallSolid").unwrap();
        assert_eq!(world.entity_name(wall), "сплошная стена");
        // Описания у прототипа нет: оно берётся у родителя.
        assert_eq!(
            world.entity_description(wall).as_deref(),
            Some("Не пройти.")
        );
        let rusty = world.spawn_prototype("WallSolidRust").unwrap();
        assert_eq!(world.entity_name(rusty), "сплошная стена");
        assert_eq!(world.entity_description(rusty).as_deref(), Some("Ржавая."));
        // Без перевода — поля прототипа, без них — идентификатор.
        let old = world.spawn_prototype("OldStyle").unwrap();
        assert_eq!(world.entity_name(old), "старое имя");
        assert_eq!(
            world.entity_description(old).as_deref(),
            Some("описание из прототипа")
        );
        // Признаки грамматики: из перевода с родителей, поверх — компонент `Grammar`.
        let arg = world.entity_arg(rusty);
        assert_eq!(arg.name, "сплошная стена");
        assert_eq!(arg.gender(), "female");
        world.entity_mut(rusty).insert(crate::Grammar {
            attributes: [("proper".to_owned(), "true".to_owned())].into(),
        });
        let arg = world.entity_arg(rusty);
        assert!(arg.proper());
        assert_eq!(arg.gender(), "female");
        let mystery = world.spawn_prototype("Mystery").unwrap();
        assert_eq!(world.entity_name(mystery), "Mystery");
        assert_eq!(world.entity_description(mystery), None);
        // Своё имя сущности важнее названия прототипа.
        world.get_mut::<MetaData>(wall).unwrap().name = Some("стена Юриста".into());
        assert_eq!(world.entity_name(wall), "стена Юриста");
    }
}
