//! Сетевые состояния компонентов движка: метаданные, положение, карта, грид, контейнеры.

use astrocore_ecs::{LifecycleWorldExt, MetaData};
use astrocore_map::{
    ChunkPos, Container, ContainerKind, ContainerManager, ContainerWorldExt, Map, MapGrid, MapId,
    MapWorldExt, Placement, Tile, TileCoord, Transform, TransformWorldExt,
};
use astrocore_math::{Angle, Vec2};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

use crate::ids::{NetEntities, NetEntity};
use crate::registry::{NetComponent, NetError, WriteContext};

fn encode<T: Serialize>(value: &T, out: &mut Vec<u8>) {
    // Запись в `Vec` не отказывает: ошибку даёт только тип, который не сериализуется.
    if let Ok(bytes) = postcard::to_extend(value, std::mem::take(out)) {
        *out = bytes;
    }
}

fn local(world: &World, net: NetEntity) -> Option<Entity> {
    world.get_resource::<NetEntities>()?.entity(net)
}

fn net_of(world: &World, entity: Entity) -> Option<NetEntity> {
    world.get::<NetEntity>(entity).copied()
}

#[derive(Serialize, Deserialize)]
struct MetaState {
    name: Option<String>,
    description: Option<String>,
    paused: bool,
}

impl NetComponent for MetaData {
    const NAME: &'static str = "MetaData";

    fn write(&self, _: &World, _: Entity, _: &WriteContext, out: &mut Vec<u8>) {
        encode(
            &MetaState {
                name: self.name.clone(),
                description: self.description.clone(),
                paused: self.paused(),
            },
            out,
        );
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let state: MetaState = postcard::from_bytes(bytes)?;
        if let Some(mut meta) = world.get_mut::<MetaData>(entity) {
            meta.name = state.name;
            meta.description = state.description;
        }
        world.set_entity_paused(entity, state.paused);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct TransformState {
    parent: Option<NetEntity>,
    position: (f32, f32),
    rotation: f32,
    level: i16,
    anchored: bool,
    no_rot: bool,
}

impl NetComponent for Transform {
    const NAME: &'static str = "Transform";

    fn write(&self, world: &World, entity: Entity, _: &WriteContext, out: &mut Vec<u8>) {
        let position = self.local_position();
        encode(
            &TransformState {
                parent: world.parent(entity).and_then(|p| net_of(world, p)),
                position: (position.x, position.y),
                rotation: self.local_rotation().0,
                level: self.level(),
                anchored: self.anchored(),
                no_rot: self.no_rot(),
            },
            out,
        );
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let state: TransformState = postcard::from_bytes(bytes)?;
        if world.get::<Transform>(entity).is_none() {
            world.entity_mut(entity).insert(Transform::default());
        }
        // Родитель вне видимости клиента: сущность временно нигде не находится.
        let parent = state.parent.and_then(|net| local(world, net));
        let _ = world.set_placement(
            entity,
            Placement {
                parent,
                position: Vec2::new(state.position.0, state.position.1),
                rotation: Angle(state.rotation),
                level: state.level,
                anchored: state.anchored && parent.is_some(),
                no_rot: state.no_rot,
            },
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct MapState {
    id: MapId,
    initialized: bool,
    paused: bool,
}

impl NetComponent for Map {
    const NAME: &'static str = "Map";

    fn write(&self, _: &World, _: Entity, _: &WriteContext, out: &mut Vec<u8>) {
        encode(
            &MapState {
                id: self.id,
                initialized: self.initialized,
                paused: self.paused,
            },
            out,
        );
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let state: MapState = postcard::from_bytes(bytes)?;
        world.adopt_map(entity, state.id, state.initialized, state.paused);
        Ok(())
    }
}

/// Чанк грида в сети: тайлы или отметка, что чанк опустел.
#[derive(Serialize, Deserialize)]
struct ChunkState {
    position: ChunkPos,
    tiles: Option<Vec<Tile>>,
}

impl NetComponent for MapGrid {
    const NAME: &'static str = "MapGrid";

    fn write(&self, _: &World, _: Entity, ctx: &WriteContext, out: &mut Vec<u8>) {
        // Клиент, уже знающий грид, получает только изменившиеся чанки.
        let chunks: Vec<ChunkState> = self
            .chunks_modified_since(ctx.since)
            .map(|(position, tiles)| ChunkState {
                position,
                tiles: tiles.map(|tiles| tiles.to_vec()),
            })
            .collect();
        encode(&chunks, out);
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let chunks: Vec<ChunkState> = postcard::from_bytes(bytes)?;
        world.ensure_grid(entity);
        let mut tiles = Vec::new();
        for chunk in chunks {
            match chunk.tiles {
                Some(values) => tiles.extend(
                    values
                        .into_iter()
                        .enumerate()
                        .map(|(index, tile)| (chunk.position.tile(index), tile)),
                ),
                None => tiles.extend(
                    (0..astrocore_map::CHUNK_TILES)
                        .map(|index| (chunk.position.tile(index), Tile::EMPTY)),
                ),
            }
        }
        let tiles: Vec<(TileCoord, Tile)> = tiles;
        world.set_tiles(entity, tiles);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ContainerState {
    id: String,
    slot: bool,
    show_ents: bool,
    occludes_light: bool,
    entities: Vec<NetEntity>,
}

impl NetComponent for ContainerManager {
    const NAME: &'static str = "ContainerContainer";
    // Содержимое контейнера создаётся позже владельца: состояние применяется, когда все
    // сущности сообщения уже существуют.
    const DEFERRED: bool = true;

    fn write(&self, world: &World, _: Entity, _: &WriteContext, out: &mut Vec<u8>) {
        let containers: Vec<ContainerState> = self
            .containers
            .iter()
            .map(|(id, container)| ContainerState {
                id: id.clone(),
                slot: container.kind == ContainerKind::Slot,
                show_ents: container.show_ents,
                occludes_light: container.occludes_light,
                entities: container
                    .entities()
                    .iter()
                    .filter_map(|e| net_of(world, *e))
                    .collect(),
            })
            .collect();
        encode(&containers, out);
    }

    fn apply(world: &mut World, entity: Entity, bytes: &[u8]) -> Result<(), NetError> {
        let containers: Vec<ContainerState> = postcard::from_bytes(bytes)?;
        world.ensure_comp::<ContainerManager>(entity);
        for state in containers {
            let kind = if state.slot {
                ContainerKind::Slot
            } else {
                ContainerKind::List
            };
            if let Some(mut manager) = world.get_mut::<ContainerManager>(entity) {
                let container = manager
                    .containers
                    .entry(state.id.clone())
                    .or_insert_with(|| Container::new(kind));
                container.kind = kind;
                container.show_ents = state.show_ents;
                container.occludes_light = state.occludes_light;
            }
            let entities: Vec<Entity> = state
                .entities
                .iter()
                .filter_map(|net| local(world, *net))
                .collect();
            world.set_container_contents(entity, &state.id, &entities);
        }
        Ok(())
    }
}
