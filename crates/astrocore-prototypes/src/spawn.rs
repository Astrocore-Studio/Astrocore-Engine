//! Создание сущностей из прототипов. Образец — `EntityManager.Spawn.cs` и
//! `EntityPrototype.LoadEntity` Robust @ 1afd692.

use std::path::Path;

use astrocore_data::DataNode;
use astrocore_ecs::{ComponentRegistry, LifecycleWorldExt, MetaData};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use crate::inherit::{FieldRules, merge_nodes};
use crate::manager::{LoadReport, Prototypes};

/// Почему сущность не создана.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    #[error("неизвестный прототип сущности {0}")]
    UnknownPrototype(String),
    #[error("прототип {prototype}, компонент {component}: {message}")]
    BadOverride {
        prototype: String,
        component: String,
        message: String,
    },
}

/// Изменения компонентов конкретной сущности относительно прототипа — запись сущности
/// в файле карты.
#[derive(Debug, Clone, Default)]
pub struct EntityOverrides<'a> {
    /// Компонент → поля, которые отличаются от прототипа; компонента может не быть
    /// в прототипе.
    pub components: Vec<(&'a str, &'a DataNode)>,
    /// Компоненты прототипа, которых у сущности нет.
    pub missing: Vec<&'a str>,
}

/// Прототипы на `World`.
pub trait PrototypeWorldExt {
    /// Загружает прототипы из каталога и разрешает наследование.
    fn load_prototypes(&mut self, dir: &Path) -> LoadReport;

    /// Создаёт сущность из прототипа в стадии `Spawned`: компоненты добавлены, события
    /// `ComponentAdd` подняты. Инициализирует сущность вызывающий, когда задаст ей место.
    fn spawn_prototype(&mut self, id: &str) -> Result<Entity, SpawnError>;

    /// То же с изменениями компонентов из файла карты.
    fn spawn_prototype_with(
        &mut self,
        id: &str,
        overrides: &EntityOverrides<'_>,
    ) -> Result<Entity, SpawnError>;
}

impl PrototypeWorldExt for World {
    fn load_prototypes(&mut self, dir: &Path) -> LoadReport {
        let mut report = LoadReport::default();
        self.init_resource::<Prototypes>();
        self.init_resource::<ComponentRegistry>();
        self.resource_scope(|world, mut prototypes: bevy_ecs::world::Mut<Prototypes>| {
            prototypes.load_dir(dir, &mut report);
            prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
        });
        report
    }

    fn spawn_prototype(&mut self, id: &str) -> Result<Entity, SpawnError> {
        self.spawn_prototype_with(id, &EntityOverrides::default())
    }

    fn spawn_prototype_with(
        &mut self,
        id: &str,
        overrides: &EntityOverrides<'_>,
    ) -> Result<Entity, SpawnError> {
        let prototype = self
            .get_resource::<Prototypes>()
            .and_then(|p| p.entity(id))
            .cloned()
            .ok_or_else(|| SpawnError::UnknownPrototype(id.to_owned()))?;

        // Изменённые компоненты разбираются до создания сущности: ошибка не оставит мусора.
        let mut changed = Vec::new();
        if !overrides.components.is_empty() {
            let registry = self.resource::<ComponentRegistry>();
            for &(name, node) in &overrides.components {
                let Some(info) = registry.get(name) else {
                    continue;
                };
                let merged = match prototype.component(name) {
                    Some(base) => merge_nodes(
                        node,
                        &base.node,
                        FieldRules {
                            always: info.inherit_always,
                            never: info.inherit_never,
                        },
                    ),
                    None => node.clone(),
                };
                let parsed = info.parse(&merged).map_err(|e| SpawnError::BadOverride {
                    prototype: id.to_owned(),
                    component: name.to_owned(),
                    message: e.to_string(),
                })?;
                changed.push((name, info.id, parsed.component));
            }
        }

        let entity = self.spawn_uninitialized();
        if let Some(mut meta) = self.get_mut::<MetaData>(entity) {
            meta.prototype = Some(prototype.id.clone());
        }
        let mut added = Vec::with_capacity(prototype.components.len() + changed.len());
        for component in &prototype.components {
            let name = &*component.name;
            if overrides.missing.contains(&name) || changed.iter().any(|(n, ..)| *n == name) {
                continue;
            }
            if let (Some(parsed), Some(component_id)) = (&component.parsed, component.component_id)
            {
                parsed.insert_clone(self, entity);
                added.push(component_id);
            }
        }
        for (_, component_id, component) in &changed {
            component.insert_clone(self, entity);
            added.push(*component_id);
        }
        for component_id in added {
            self.notify_comp_added(entity, component_id);
        }
        Ok(entity)
    }
}
