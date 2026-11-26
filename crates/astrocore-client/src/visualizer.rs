//! Визуализатор по данным: значения внешнего вида сущности меняют слои её спрайта.
//! Образец — `GenericVisualizerComponent` и `GenericVisualizerSystem` SS14 @ 2f4a2aad.

use std::collections::HashMap;

use astrocore_ecs::{ComponentInit, EventWorldExt, RegistryWorldExt};
use astrocore_math::{Angle, Color, Vec2};
use astrocore_net::{Appearance, AppearanceChange};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::sprite::{Sprite, SpriteLayer};

/// Изменения слоя спрайта: заданные поля заменяют поля слоя (`PrototypeLayerData` Robust).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpriteLayerData {
    pub sprite: Option<String>,
    pub state: Option<String>,
    pub texture: Option<String>,
    pub color: Option<Color>,
    pub visible: Option<bool>,
    pub scale: Option<Vec2>,
    pub offset: Option<Vec2>,
    pub rotation: Option<Angle>,
    pub shader: Option<String>,
}

impl SpriteLayerData {
    /// Применяет изменения к слою.
    pub fn apply(&self, layer: &mut SpriteLayer) {
        if let Some(sprite) = &self.sprite {
            layer.sprite = Some(sprite.clone());
        }
        if let Some(state) = &self.state {
            layer.state = Some(state.clone());
        }
        if let Some(texture) = &self.texture {
            layer.texture = Some(texture.clone());
        }
        if let Some(color) = self.color {
            layer.color = color;
        }
        if let Some(visible) = self.visible {
            layer.visible = visible;
        }
        if let Some(scale) = self.scale {
            layer.scale = scale;
        }
        if let Some(offset) = self.offset {
            layer.offset = offset;
        }
        if let Some(rotation) = self.rotation {
            layer.rotation = rotation;
        }
        if let Some(shader) = &self.shader {
            layer.shader = Some(shader.clone());
        }
    }
}

impl Sprite {
    /// Номер слоя по ключу из `map` слоя; ключ-число — номер слоя (`LayerMapTryGet` Robust).
    pub fn layer_index(&self, key: &str) -> Option<usize> {
        self.layers
            .iter()
            .position(|layer| layer.map.iter().any(|k| k == key))
            .or_else(|| key.parse().ok().filter(|index| *index < self.layers.len()))
    }

    /// Меняет слой с ключом. Возвращает `false`, если такого слоя нет.
    pub fn layer_set_data(&mut self, key: &str, data: &SpriteLayerData) -> bool {
        let Some(index) = self.layer_index(key) else {
            return false;
        };
        data.apply(&mut self.layers[index]);
        true
    }
}

/// Правила: ключ внешнего вида → ключ слоя → текст значения → изменения слоя. В данных —
/// компонент `GenericVisualizer`.
#[derive(Component, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenericVisualizer {
    #[serde(default)]
    pub visuals: HashMap<String, HashMap<String, HashMap<String, SpriteLayerData>>>,
}

fn apply_visuals(world: &mut World, entity: Entity) {
    let (Some(visualizer), Some(appearance), Some(sprite)) = (
        world.get::<GenericVisualizer>(entity),
        world.get::<Appearance>(entity),
        world.get::<Sprite>(entity),
    ) else {
        return;
    };
    let mut updated = sprite.clone();
    for (key, layers) in &visualizer.visuals {
        let Some(value) = appearance.get(key) else {
            continue;
        };
        let value = value.to_string();
        for (layer, by_value) in layers {
            if let Some(data) = by_value.get(&value) {
                updated.layer_set_data(layer, data);
            }
        }
    }
    // Спрайт помечается изменённым, только если он правда другой: клиент перестраивает
    // картинку сущности по отметке изменения.
    if updated != *sprite
        && let Some(mut sprite) = world.get_mut::<Sprite>(entity)
    {
        *sprite = updated;
    }
}

/// Подключает визуализатор к миру клиента.
pub fn install(world: &mut World) {
    world.register_data_component::<GenericVisualizer>();
    world.subscribe::<GenericVisualizer, AppearanceChange>(|world, entity, _| {
        apply_visuals(world, entity);
    });
    world.subscribe::<GenericVisualizer, ComponentInit>(|world, entity, _| {
        apply_visuals(world, entity);
    });
}

#[cfg(test)]
mod tests {
    use astrocore_data::DataFormat;
    use astrocore_ecs::{ComponentRegistry, GameWorldExt};
    use astrocore_net::{AppearanceWorldExt, NetClientPlugin};
    use astrocore_prototypes::{LoadReport, PrototypeWorldExt, Prototypes};

    use super::*;

    const PROTOTYPES: &str = r##"
- type: entity
  id: Light
  components:
  - type: Appearance
  - type: Sprite
    sprite: Structures/Wallmounts/Lighting/light_tube.rsi
    layers:
    - state: base
    - state: glow
      map: ["enum.PoweredLightLayers.Glow"]
      visible: false
  - type: GenericVisualizer
    visuals:
      enum.PoweredLightVisuals.BulbState:
        enum.PoweredLightLayers.Glow:
          On: { visible: true, state: glow }
          Off: { visible: false }
          Broken: { visible: true, state: broken, color: "#FF0000" }
      enum.PowerDeviceVisuals.Powered:
        0:
          True: { state: base }
          False: { state: off }
"##;

    #[test]
    fn appearance_values_change_layers() {
        let mut world = World::new();
        world.add_plugin(NetClientPlugin);
        world.register_data_component::<Sprite>();
        install(&mut world);
        let mut prototypes = Prototypes::default();
        let mut report = LoadReport::default();
        prototypes.load_str("test.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
        prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
        assert_eq!(report.errors, []);
        world.insert_resource(prototypes);

        let light = world.spawn_prototype("Light").unwrap();
        let layers = |world: &World| world.get::<Sprite>(light).unwrap().layers.clone();
        assert!(!layers(&world)[1].visible);

        world.set_appearance(light, "enum.PoweredLightVisuals.BulbState", "On");
        assert!(layers(&world)[1].visible);
        assert_eq!(layers(&world)[1].state.as_deref(), Some("glow"));

        world.set_appearance(light, "enum.PoweredLightVisuals.BulbState", "Broken");
        assert_eq!(layers(&world)[1].state.as_deref(), Some("broken"));
        assert_eq!(layers(&world)[1].color.to_bytes(), [255, 0, 0, 255]);

        // Слой по номеру и логическое значение.
        world.set_appearance(light, "enum.PowerDeviceVisuals.Powered", false);
        assert_eq!(layers(&world)[0].state.as_deref(), Some("off"));

        // Значение без правила ничего не меняет.
        let before = layers(&world);
        world.set_appearance(light, "enum.PoweredLightVisuals.BulbState", "Empty");
        assert_eq!(layers(&world), before);
    }
}
