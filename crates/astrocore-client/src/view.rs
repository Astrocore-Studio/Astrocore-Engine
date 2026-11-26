//! Отрисовка мира клиента: сущности игры → спрайты Bevy, тайлы гридов, камера.
//!
//! Мир игры (сущности, сеть, предсказание) живёт в ресурсе [`Game`] отдельно от мира
//! отрисовки Bevy. Каждый кадр видимые сущности игры получают спрайты: слой спрайта —
//! дочерняя сущность Bevy. Рисуется карта и этаж, на которых стоит сущность игрока.

use std::collections::{HashMap, HashSet};

use astrocore_ecs::MetaData;
use astrocore_map::{
    CHUNK_SIZE, ChunkPos, Map, MapGrid, TileCoord, TileDefinitions, Transform as GameTransform,
    TransformWorldExt, WorldTransform,
};
use astrocore_math::Angle;
use astrocore_net::{ClientNet, Detached};
use astrocore_resources::Resources;
use bevy::asset::RenderAssetUsages;
use bevy::camera::ScalingMode;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_ecs::change_detection::Tick;

use crate::Game;
use crate::sprite::{LayerSource, Sprite as GameSprite};
use crate::textures::{PIXELS_PER_TILE, StateTexture, Textures, rsi_direction};

/// Сколько тайлов помещается в окне по вертикали (`ViewportSize` SS14).
pub const VIEW_TILES: f32 = 15.0;

/// Глубина тайлов: ниже любых спрайтов.
const TILE_DEPTH: f32 = -500.0;

/// Спрайт сущности игры в мире отрисовки.
#[derive(Component)]
pub(crate) struct EntityView {
    /// Отметка изменения компонента `Sprite`, по которой построены слои.
    built: Option<Tick>,
}

/// Слой спрайта с состоянием RSI: выбор кадра по направлению и времени.
#[derive(Component)]
pub(crate) struct LayerView {
    texture: StateTexture,
    /// Поворот самого слоя.
    rotation: Angle,
    offset: Vec2,
    /// Номер слоя: порядок отрисовки внутри сущности.
    order: f32,
}

/// Картинка чанка грида.
#[derive(Component)]
pub(crate) struct ChunkView;

/// Соответствие сущностей игры и их изображений.
#[derive(Resource, Default)]
pub struct Views {
    entities: HashMap<bevy_ecs::entity::Entity, Entity>,
    chunks: HashMap<(bevy_ecs::entity::Entity, ChunkPos), (Entity, astrocore_ecs::GameTick)>,
    /// Декодированные картинки тайлов: номер определения → пиксели RGBA и ширина.
    tiles: HashMap<u16, Option<(Vec<u8>, u32)>>,
}

impl Views {
    /// Сколько сущностей игры сейчас нарисовано.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Сколько чанков гридов сейчас нарисовано.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Глаз игрока: карта, положение и этаж.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Eye {
    pub map: Option<bevy_ecs::entity::Entity>,
    pub position: Vec2,
    pub level: i16,
}

pub(crate) fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: VIEW_TILES,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));
}

/// Глаз следует за сущностью игрока, камера — за глазом.
pub(crate) fn follow_player(
    game: Res<Game>,
    mut eye: ResMut<Eye>,
    mut camera: Single<&mut Transform, With<Camera2d>>,
) {
    let world = &game.0;
    let Some(entity) = world.resource::<ClientNet>().attached else {
        return;
    };
    let (Some(transform), Some(placement)) = (
        world.get::<GameTransform>(entity),
        world.world_transform(entity),
    ) else {
        return;
    };
    *eye = Eye {
        map: transform.map(),
        position: Vec2::new(placement.position.x, placement.position.y),
        level: placement.level,
    };
    camera.translation.x = eye.position.x;
    camera.translation.y = eye.position.y;
}

fn bevy_color(color: astrocore_math::Color) -> Color {
    Color::srgba(color.r, color.g, color.b, color.a)
}

/// Положение сущности на экране: мировые координаты и глубина.
fn screen_transform(placement: &WorldTransform, depth: i32, upright: bool) -> Transform {
    // Сущности одной глубины перекрываются по Y: нижняя рисуется поверх верхней.
    let z = depth as f32 - placement.position.y * 1e-4;
    let rotation = if upright { 0.0 } else { placement.rotation.0 };
    Transform::from_xyz(placement.position.x, placement.position.y, z)
        .with_rotation(Quat::from_rotation_z(rotation))
}

/// Создаёт, обновляет и убирает спрайты сущностей игры.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_entities(
    mut commands: Commands,
    game: Res<Game>,
    eye: Res<Eye>,
    resources: Res<GameResources>,
    mut views: ResMut<Views>,
    mut textures: ResMut<Textures>,
    mut images: ResMut<Assets<Image>>,
    mut layouts: ResMut<Assets<TextureAtlasLayout>>,
    mut placed: Query<(&mut Transform, &mut EntityView, &mut Visibility)>,
) {
    let world = &game.0;
    let mut alive: HashSet<bevy_ecs::entity::Entity> = HashSet::with_capacity(views.entities.len());
    let Some(sprite_id) = world.component_id::<GameSprite>() else {
        return;
    };
    for archetype in world.archetypes().iter() {
        if !archetype.contains(sprite_id) {
            continue;
        }
        for archetype_entity in archetype.entities() {
            let entity = archetype_entity.id();
            let Ok(entity_ref) = world.get_entity(entity) else {
                continue;
            };
            let (Some(sprite), Some(transform)) = (
                entity_ref.get::<GameSprite>(),
                entity_ref.get::<GameTransform>(),
            ) else {
                continue;
            };
            // Рисуются сущности карты и этажа глаза; сущности в контейнерах не рисуются.
            if entity_ref.contains::<Detached>() || transform.map() != eye.map || eye.map.is_none()
            {
                continue;
            }
            let top_level = world
                .parent(entity)
                .is_some_and(|parent| Some(parent) == transform.grid().or(transform.map()));
            if !top_level {
                continue;
            }
            let Some(placement) = world.world_transform(entity) else {
                continue;
            };
            if placement.level != eye.level {
                continue;
            }
            alive.insert(entity);
            let target = screen_transform(&placement, sprite.drawdepth.0, sprite.no_rot);
            let changed = entity_ref
                .get_change_ticks::<GameSprite>()
                .map(|t| t.changed);
            let view = match views.entities.get(&entity).copied() {
                Some(view) => view,
                None => {
                    let view = commands
                        .spawn((target, Visibility::Inherited, EntityView { built: None }))
                        .id();
                    views.entities.insert(entity, view);
                    view
                }
            };
            let rebuild = match placed.get_mut(view) {
                Ok((mut current, mut state, mut visibility)) => {
                    if *current != target {
                        *current = target;
                    }
                    if *visibility != Visibility::Inherited {
                        *visibility = Visibility::Inherited;
                    }
                    let rebuild = state.built != changed;
                    state.built = changed;
                    rebuild
                }
                // Изображение создано в этом кадре: слои появятся в следующем.
                Err(_) => false,
            };
            if !rebuild {
                continue;
            }
            commands.entity(view).despawn_related::<Children>();
            for (order, layer) in sprite.resolved_layers().into_iter().enumerate() {
                let order = order as f32 * 1e-3;
                let color = bevy_color(layer.color);
                match &layer.source {
                    LayerSource::Rsi { rsi, state } => {
                        let Some(texture) =
                            textures.state(&resources.0, &mut images, &mut layouts, rsi, state)
                        else {
                            continue;
                        };
                        commands.spawn((
                            ChildOf(view),
                            Sprite {
                                image: texture.image.clone(),
                                texture_atlas: Some(TextureAtlas {
                                    layout: texture.layout.clone(),
                                    index: 0,
                                }),
                                color,
                                custom_size: Some(texture.size * layer.scale),
                                ..default()
                            },
                            Transform::from_xyz(layer.offset.x, layer.offset.y, order),
                            LayerView {
                                texture,
                                rotation: layer.rotation,
                                offset: layer.offset,
                                order,
                            },
                        ));
                    }
                    LayerSource::Texture(path) => {
                        let Some((image, size)) = textures.single(&resources.0, &mut images, path)
                        else {
                            continue;
                        };
                        commands.spawn((
                            ChildOf(view),
                            Sprite {
                                image,
                                color,
                                custom_size: Some(size * layer.scale),
                                ..default()
                            },
                            Transform::from_xyz(layer.offset.x, layer.offset.y, order)
                                .with_rotation(Quat::from_rotation_z(layer.rotation.0)),
                        ));
                    }
                }
            }
        }
    }
    // Сущности, которых больше не видно.
    views.entities.retain(|entity, view| {
        let keep = alive.contains(entity);
        if !keep {
            commands.entity(*view).despawn();
        }
        keep
    });
}

/// Выбирает кадр слоя по направлению сущности и времени анимации.
pub(crate) fn animate_layers(
    time: Res<Time>,
    parents: Query<&Transform, (With<EntityView>, Without<LayerView>)>,
    mut layers: Query<(&ChildOf, &LayerView, &mut Sprite, &mut Transform)>,
) {
    let now = time.elapsed_secs();
    for (child_of, layer, mut sprite, mut transform) in &mut layers {
        let Ok(parent) = parents.get(child_of.parent()) else {
            continue;
        };
        let directions = layer.texture.state.directions.count();
        // Угол сущности: поворот её изображения вокруг оси Z.
        let facing = Angle(parent.rotation.to_euler(EulerRot::ZYX).0);
        let direction = rsi_direction(directions, facing);
        let frame = layer.texture.state.frame_at(direction, now);
        let index = layer.texture.state.sheet_index(direction, frame);
        if let Some(atlas) = &mut sprite.texture_atlas
            && atlas.index != index
        {
            atlas.index = index;
        }
        // Состояние с несколькими направлениями рисуется без поворота: направление уже
        // выбрано кадром. Поворот родителя снимается обратным поворотом слоя.
        let local = if directions > 1 {
            layer.rotation.0 - facing.0
        } else {
            layer.rotation.0
        };
        let target = Transform::from_xyz(layer.offset.x, layer.offset.y, layer.order)
            .with_rotation(Quat::from_rotation_z(local));
        if *transform != target {
            *transform = target;
        }
    }
}

/// Каталоги ресурсов для чтения картинок.
#[derive(Resource, Clone)]
pub struct GameResources(pub Resources);

/// Пиксели картинки тайла: RGBA и ширина.
fn tile_pixels(
    resources: &Resources,
    definitions: &TileDefinitions,
    type_id: u16,
) -> Option<(Vec<u8>, u32)> {
    let sprite = definitions.get(type_id)?.sprite.as_ref()?;
    let bytes = resources.read(sprite.trim_start_matches('/')).ok()?;
    let image = Image::from_buffer(
        &bytes,
        bevy::image::ImageType::Extension("png"),
        bevy::image::CompressedImageFormats::NONE,
        true,
        ImageSampler::nearest(),
        RenderAssetUsages::default(),
    )
    .ok()?;
    let image = image.convert(TextureFormat::Rgba8UnormSrgb)?;
    Some((image.data?, image.texture_descriptor.size.width))
}

/// Рисует чанки гридов карты глаза: по картинке на чанк, собранной из картинок тайлов.
pub(crate) fn sync_chunks(
    mut commands: Commands,
    game: Res<Game>,
    eye: Res<Eye>,
    resources: Res<GameResources>,
    mut views: ResMut<Views>,
    mut images: ResMut<Assets<Image>>,
) {
    let world = &game.0;
    let mut alive: HashSet<(bevy_ecs::entity::Entity, ChunkPos)> = HashSet::new();
    let grids: Vec<bevy_ecs::entity::Entity> = eye
        .map
        .and_then(|map| world.get::<Map>(map))
        .map(|map| map.grids().to_vec())
        .unwrap_or_default();
    let definitions = world.resource::<TileDefinitions>();
    let side = CHUNK_SIZE as u32 * PIXELS_PER_TILE as u32;
    for grid in grids {
        let (Some(tiles), Some(placement)) =
            (world.get::<MapGrid>(grid), world.world_transform(grid))
        else {
            continue;
        };
        for (position, chunk) in tiles.chunks() {
            if i32::from(placement.level) + i32::from(position.z) != i32::from(eye.level)
                || chunk.filled() == 0
            {
                continue;
            }
            alive.insert((grid, position));
            // Центр чанка в мировых координатах с учётом положения и поворота грида.
            let half = CHUNK_SIZE as f32 / 2.0;
            let local = astrocore_math::Vec2::new(
                (position.x * CHUNK_SIZE) as f32 + half,
                (position.y * CHUNK_SIZE) as f32 + half,
            );
            let center = placement.to_world(local);
            let target = Transform::from_xyz(center.x, center.y, TILE_DEPTH)
                .with_rotation(Quat::from_rotation_z(placement.rotation.0));
            if let Some((view, modified)) = views.chunks.get(&(grid, position)).copied()
                && modified == chunk.last_modified
            {
                commands.entity(view).insert(target);
                continue;
            }
            // Сборка картинки чанка: строки картинки идут сверху вниз, строки тайлов — снизу вверх.
            let mut pixels = vec![0u8; (side * side * 4) as usize];
            let tile_px = PIXELS_PER_TILE as u32;
            for (index, tile) in chunk.tiles().iter().enumerate() {
                if tile.is_empty() {
                    continue;
                }
                let source = views
                    .tiles
                    .entry(tile.type_id)
                    .or_insert_with(|| tile_pixels(&resources.0, definitions, tile.type_id));
                let Some((source, width)) = source else {
                    continue;
                };
                let coord: TileCoord = position.tile(index);
                let (tx, ty) = (
                    coord.x.rem_euclid(CHUNK_SIZE) as u32,
                    coord.y.rem_euclid(CHUNK_SIZE) as u32,
                );
                let variants = (*width / tile_px).max(1);
                let variant = u32::from(tile.variant) % variants;
                for row in 0..tile_px {
                    let from = ((row * *width + variant * tile_px) * 4) as usize;
                    let dest_row = (CHUNK_SIZE as u32 - 1 - ty) * tile_px + row;
                    let to = ((dest_row * side + tx * tile_px) * 4) as usize;
                    let length = (tile_px * 4) as usize;
                    if from + length <= source.len() {
                        pixels[to..to + length].copy_from_slice(&source[from..from + length]);
                    }
                }
            }
            let mut image = Image::new(
                Extent3d {
                    width: side,
                    height: side,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                pixels,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            image.sampler = ImageSampler::nearest();
            let sprite = Sprite {
                image: images.add(image),
                custom_size: Some(Vec2::splat(CHUNK_SIZE as f32)),
                ..default()
            };
            let view = match views.chunks.get(&(grid, position)) {
                Some((view, _)) => {
                    commands.entity(*view).insert((sprite, target));
                    *view
                }
                None => commands.spawn((sprite, target, ChunkView)).id(),
            };
            views
                .chunks
                .insert((grid, position), (view, chunk.last_modified));
        }
    }
    views.chunks.retain(|key, (view, _)| {
        let keep = alive.contains(key);
        if !keep {
            commands.entity(*view).despawn();
        }
        keep
    });
}

/// Имя сущности для отладочных надписей.
pub fn entity_label(world: &bevy_ecs::world::World, entity: bevy_ecs::entity::Entity) -> String {
    world
        .get::<MetaData>(entity)
        .and_then(|meta| {
            meta.name
                .clone()
                .or(meta.prototype.as_ref().map(|p| p.to_string()))
        })
        .unwrap_or_else(|| format!("{entity}"))
}
