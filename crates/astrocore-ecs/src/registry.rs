//! Реестр компонентов данных: имя в прототипах и картах ↔ тип Rust.
//! Образец — `Robust.Shared/GameObjects/ComponentFactory.cs` @ 1afd692.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use astrocore_data::{DataError, DataNode, UnknownField, from_node_checked, to_node};
use bevy_ecs::component::{Component, ComponentId, Mutable};
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Значение компонента без известного типа: разобрано из данных, готово к вставке.
pub trait ErasedComponent: Send + Sync {
    /// Вставляет копию компонента в сущность без событий жизненного цикла.
    fn insert_clone(&self, world: &mut World, entity: Entity);

    fn clone_box(&self) -> Box<dyn ErasedComponent>;

    /// Значение для приведения к известному типу.
    fn as_any(&self) -> &dyn Any;
}

impl<T: Component + Clone> ErasedComponent for T {
    fn insert_clone(&self, world: &mut World, entity: Entity) {
        if let Ok(mut target) = world.get_entity_mut(entity) {
            target.insert(self.clone());
        }
    }

    fn clone_box(&self) -> Box<dyn ErasedComponent> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Компонент, разобранный из данных, и поля, которых у него нет.
pub struct ParsedComponent {
    pub component: Box<dyn ErasedComponent>,
    pub unknown: Vec<UnknownField>,
}

/// Запись реестра об одном типе компонента.
#[derive(Clone)]
pub struct ComponentInfo {
    /// Имя в данных: `Sprite` для `- type: Sprite`.
    pub name: Arc<str>,
    pub id: ComponentId,
    pub type_id: TypeId,
    /// Поля, которые при наследовании прототипов складываются с родительскими
    /// (`AlwaysPushInheritance`).
    pub inherit_always: &'static [&'static str],
    /// Поля, которые от родительского прототипа не наследуются (`NeverPushInheritance`).
    pub inherit_never: &'static [&'static str],
    parse: fn(&DataNode) -> Result<ParsedComponent, DataError>,
    write: fn(&World, Entity) -> Option<Result<DataNode, DataError>>,
    write_value: fn(&dyn ErasedComponent) -> Option<Result<DataNode, DataError>>,
}

impl ComponentInfo {
    /// Разбирает поля компонента из узла данных.
    pub fn parse(&self, node: &DataNode) -> Result<ParsedComponent, DataError> {
        (self.parse)(node)
    }

    /// Записывает компонент сущности в узел данных; `None` — компонента нет.
    pub fn write(&self, world: &World, entity: Entity) -> Option<Result<DataNode, DataError>> {
        (self.write)(world, entity)
    }

    /// Записывает разобранное значение (например, компонент прототипа); `None` — значение
    /// другого типа.
    pub fn write_value(&self, value: &dyn ErasedComponent) -> Option<Result<DataNode, DataError>> {
        (self.write_value)(value)
    }
}

/// Реестр компонентов данных. Ресурс `World`.
#[derive(Resource, Default)]
pub struct ComponentRegistry {
    list: Vec<ComponentInfo>,
    by_name: HashMap<Arc<str>, usize>,
    by_id: HashMap<ComponentId, usize>,
}

impl ComponentRegistry {
    pub fn get(&self, name: &str) -> Option<&ComponentInfo> {
        self.by_name.get(name).map(|&index| &self.list[index])
    }

    pub fn get_by_id(&self, id: ComponentId) -> Option<&ComponentInfo> {
        self.by_id.get(&id).map(|&index| &self.list[index])
    }

    /// Задаёт правила наследования полей компонента в прототипах.
    pub fn set_inheritance(
        &mut self,
        name: &str,
        always: &'static [&'static str],
        never: &'static [&'static str],
    ) {
        if let Some(&index) = self.by_name.get(name) {
            self.list[index].inherit_always = always;
            self.list[index].inherit_never = never;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &ComponentInfo> {
        self.list.iter()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Имя компонента по имени типа: последний сегмент пути без суффикса `Component`.
pub fn component_name<T: 'static>() -> &'static str {
    let full = std::any::type_name::<T>();
    let base = full.split('<').next().unwrap_or(full);
    let short = base.rsplit("::").next().unwrap_or(base);
    match short.strip_suffix("Component") {
        Some(stripped) if !stripped.is_empty() => stripped,
        _ => short,
    }
}

fn parse_as<T: Component + Clone + DeserializeOwned>(
    node: &DataNode,
) -> Result<ParsedComponent, DataError> {
    let (component, unknown) = from_node_checked::<T>(node)?;
    Ok(ParsedComponent {
        component: Box::new(component),
        unknown,
    })
}

fn write_as<T: Component + Serialize>(
    world: &World,
    entity: Entity,
) -> Option<Result<DataNode, DataError>> {
    world.get::<T>(entity).map(to_node)
}

fn write_value_as<T: Component + Serialize>(
    value: &dyn ErasedComponent,
) -> Option<Result<DataNode, DataError>> {
    value.as_any().downcast_ref::<T>().map(to_node)
}

/// Регистрация компонентов данных на `World`.
pub trait RegistryWorldExt {
    /// Регистрирует компонент под именем его типа (`DoorComponent` и `Door` → `Door`).
    fn register_data_component<T>(&mut self) -> ComponentId
    where
        T: Component<Mutability = Mutable> + Clone + Serialize + DeserializeOwned;

    /// Регистрирует компонент под заданным именем.
    fn register_data_component_as<T>(&mut self, name: &str) -> ComponentId
    where
        T: Component<Mutability = Mutable> + Clone + Serialize + DeserializeOwned;
}

impl RegistryWorldExt for World {
    fn register_data_component<T>(&mut self) -> ComponentId
    where
        T: Component<Mutability = Mutable> + Clone + Serialize + DeserializeOwned,
    {
        self.register_data_component_as::<T>(component_name::<T>())
    }

    fn register_data_component_as<T>(&mut self, name: &str) -> ComponentId
    where
        T: Component<Mutability = Mutable> + Clone + Serialize + DeserializeOwned,
    {
        let id = self.register_component::<T>();
        let mut registry = self.get_resource_or_insert_with(ComponentRegistry::default);
        if let Some(&index) = registry.by_name.get(name) {
            let existing = &registry.list[index];
            assert!(
                existing.type_id == TypeId::of::<T>(),
                "имя компонента `{name}` уже занято другим типом"
            );
            return id;
        }
        let name: Arc<str> = Arc::from(name);
        let index = registry.list.len();
        registry.list.push(ComponentInfo {
            name: name.clone(),
            id,
            type_id: TypeId::of::<T>(),
            inherit_always: &[],
            inherit_never: &[],
            parse: parse_as::<T>,
            write: write_as::<T>,
            write_value: write_value_as::<T>,
        });
        registry.by_name.insert(name, index);
        registry.by_id.insert(id, index);
        id
    }
}

#[cfg(test)]
mod tests {
    use astrocore_data::parse_yaml;
    use serde::Deserialize;

    use super::*;

    #[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct DoorComponent {
        open_time: f32,
        bolted: bool,
    }

    #[test]
    fn name_comes_from_type() {
        assert_eq!(component_name::<DoorComponent>(), "Door");
        assert_eq!(component_name::<Vec<DoorComponent>>(), "Vec");
    }

    #[test]
    fn parses_inserts_and_writes_back() {
        let mut world = World::new();
        let id = world.register_data_component::<DoorComponent>();
        assert_eq!(world.register_data_component::<DoorComponent>(), id);

        let registry = world.resource::<ComponentRegistry>();
        let info = registry.get("Door").unwrap().clone();
        assert_eq!(registry.get_by_id(id).unwrap().name.as_ref(), "Door");

        let node = parse_yaml("type: Door\nopenTime: 1.5\ncolor: red\n").unwrap();
        let parsed = info.parse(&node).unwrap();
        // Ключ `type` — служебный; его отбрасывает загрузчик прототипов.
        let unknown: Vec<_> = parsed.unknown.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(unknown, ["type", "color"]);

        let entity = world.spawn_empty().id();
        parsed.component.insert_clone(&mut world, entity);
        parsed
            .component
            .clone_box()
            .insert_clone(&mut world, entity);
        assert_eq!(
            world.get::<DoorComponent>(entity),
            Some(&DoorComponent {
                open_time: 1.5,
                bolted: false
            })
        );
        let written = info.write(&world, entity).unwrap().unwrap();
        assert_eq!(written.get("openTime").unwrap().as_str(), Some("1.5"));
        let bare = world.spawn_empty().id();
        assert!(info.write(&world, bare).is_none());
    }
}
