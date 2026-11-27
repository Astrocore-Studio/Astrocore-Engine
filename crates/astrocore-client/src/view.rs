//! Отрисовка мира клиента: сущности игры → спрайты Bevy, тайлы гридов, камера.
//!
//! Мир игры (сущности, сеть, предсказание) живёт в ресурсе [`Game`] отдельно от мира
//! отрисовки Bevy. Каждый кадр видимые сущности игры получают спрайты: слой спрайта —
//! дочерняя сущность Bevy. Рисуется карта и этаж, на которых стоит сущность игрока.

use std::collections::{HashMap, HashSet};

use astrocore_ecs::{GameTick, GameTime};
use astrocore_loc::EntityNameWorldExt;
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
use crate::unshaded::UNSHADED_LAYER;
use bevy::camera::visibility::RenderLayers;

/// Сколько тайлов помещается в окне по вертикали (`ViewportSize` SS14).
pub const VIEW_TILES: f32 = 15.0;

/// Глубина тайлов: ниже любых спрайтов.
const TILE_DEPTH: f32 = -500.0;

/// Спрайт сущности игры в мире отрисовки.
#[derive(Component)]
pub(crate) struct EntityView {
    /// Отметка изменения компонента `Sprite`, по которой построены слои.
    built: Option<Tick>,
    /// Положение на прошлом такте мира и на текущем: между ними кадры сглаживают движение.
    from: Vec2,
    to: Vec2,
    /// Такт мира, на котором записано положение `to`.
    tick: GameTick,
}

/// Сдвиг за один такт, после которого сущность считается перенесённой, а не пришедшей:
/// сглаживание такого сдвига выглядело бы как полёт через карту.
const TELEPORT_DISTANCE: f32 = 2.0;

impl EntityView {
    fn new(target: &Transform, tick: GameTick) -> Self {
        let position = target.translation.truncate();
        Self {
            built: None,
            from: position,
            to: position,
            tick,
        }
    }

    /// Положение для кадра: `alpha` — доля такта, прошедшая с его начала.
    fn interpolate(&mut self, target: &Transform, tick: GameTick, alpha: f32) -> Transform {
        let to = target.translation.truncate();
        if self.tick != tick {
            self.tick = tick;
            self.from = if self.to.distance(to) > TELEPORT_DISTANCE {
                to
            } else {
                self.to
            };
            self.to = to;
        }
        let position = self.from.lerp(self.to, alpha);
        Transform {
            translation: position.extend(target.translation.z),
            ..*target
        }
    }
}

/// Слой спрайта с состоянием RSI: выбор кадра по направлению и времени.
#[derive(Component, Clone)]
pub(crate) struct LayerView {
    texture: StateTexture,
    /// Поворот самого слоя.
    rotation: Angle,
    offset: Vec2,
    /// Номер слоя: порядок отрисовки внутри сущности.
    order: f32,
    /// Направление набора, заданное слоем.
    direction: Option<u8>,
    /// Время начала анимации слоя, секунды часов кадра.
    start: f32,
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
    /// Картинки краёв тайлов: путь → пиксели RGBA и ширина.
    edges: HashMap<String, Option<(Vec<u8>, u32)>>,
    /// Перезапуски анимаций слоёв: (сущность, слой) → (номер запуска, время начала).
    restarts: HashMap<(bevy_ecs::entity::Entity, usize), (u32, f32)>,
    /// Наименьшая глубина, на которой встретился светящийся слой: обычные слои с глубиной
    /// не ниже получают двойников в маске светящегося (ADR-0016).
    unshaded_depth: Option<i32>,
    /// Глубина опустилась: в следующем кадре слои всех сущностей строятся заново.
    rebuild_all: bool,
}

impl Views {
    /// Сколько сущностей игры сейчас нарисовано.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Сущности игры и их изображения.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (bevy_ecs::entity::Entity, Entity)> + '_ {
        self.entities.iter().map(|(entity, view)| (*entity, *view))
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

/// Камера мира, которую видит игрок (в отличие от камеры маски светящегося).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WorldCamera;

/// Проекция камеры мира: окно по вертикали вмещает [`VIEW_TILES`] тайлов.
pub(crate) fn world_projection() -> Projection {
    Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::FixedVertical {
            viewport_height: VIEW_TILES,
        },
        ..OrthographicProjection::default_2d()
    })
}

pub(crate) fn spawn_camera(mut commands: Commands) {
    commands.spawn((WorldCamera, Camera2d, world_projection()));
}

/// Глаз следует за сущностью игрока, камера — за глазом.
pub(crate) fn follow_player(
    game: Res<Game>,
    views: Res<Views>,
    mut eye: ResMut<Eye>,
    mut camera: Single<&mut Transform, With<WorldCamera>>,
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
    // Пока у сущности игрока нет изображения, камера стоит в точке глаза; дальше её ведёт
    // `follow_view` по сглаженному положению.
    if !views.entities.contains_key(&entity) {
        camera.translation.x = eye.position.x;
        camera.translation.y = eye.position.y;
    }
}

/// Камера следует за изображением сущности игрока: оно движется плавно между тактами.
pub(crate) fn follow_view(
    game: Res<Game>,
    views: Res<Views>,
    shown: Query<&Transform, (With<EntityView>, Without<WorldCamera>)>,
    mut camera: Single<&mut Transform, With<WorldCamera>>,
) {
    let Some(entity) = game.0.resource::<ClientNet>().attached else {
        return;
    };
    if let Some(view) = views.entities.get(&entity)
        && let Ok(transform) = shown.get(*view)
    {
        camera.translation.x = transform.translation.x;
        camera.translation.y = transform.translation.y;
    }
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
    fixed: Res<Time<Fixed>>,
    clock: Res<Time>,
    mut placed: Query<(&mut Transform, &mut EntityView, &mut Visibility)>,
) {
    let world = &game.0;
    let time = world.resource::<GameTime>();
    let tick = time.tick;
    // На паузе мир стоит: кадры показывают текущее положение.
    let alpha = if time.paused {
        1.0
    } else {
        fixed.overstep_fraction()
    };
    let mut alive: HashSet<bevy_ecs::entity::Entity> = HashSet::with_capacity(views.entities.len());
    let rebuild_all = std::mem::take(&mut views.rebuild_all);
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
                        .spawn((
                            target,
                            Visibility::Inherited,
                            EntityView::new(&target, tick),
                        ))
                        .id();
                    views.entities.insert(entity, view);
                    view
                }
            };
            let rebuild = match placed.get_mut(view) {
                Ok((mut current, mut state, mut visibility)) => {
                    let shown = state.interpolate(&target, tick, alpha);
                    if *current != shown {
                        *current = shown;
                    }
                    if *visibility != Visibility::Inherited {
                        *visibility = Visibility::Inherited;
                    }
                    let rebuild = state.built != changed || rebuild_all;
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
            let layers = sprite.resolved_layers();
            let depth = sprite.drawdepth.0;
            if layers.iter().any(|layer| layer.unshaded)
                && views.unshaded_depth.is_none_or(|lowest| depth < lowest)
            {
                // Обычные слои ниже прежней границы теперь могут закрывать светящиеся.
                views.rebuild_all |= views.unshaded_depth.is_some();
                views.unshaded_depth = Some(depth);
            }
            let masked = views.unshaded_depth.is_some_and(|lowest| depth >= lowest);
            for (order, layer) in layers.into_iter().enumerate() {
                let order = order as f32 * 1e-3;
                let color = bevy_color(layer.color);
                // Двойник в маске светящегося: свой цвет или чёрный с той же прозрачностью.
                let twin = (layer.unshaded || masked).then(|| {
                    if layer.unshaded {
                        color
                    } else {
                        Color::BLACK.with_alpha(layer.color.a)
                    }
                });
                match &layer.source {
                    LayerSource::Rsi { rsi, state } => {
                        let Some(texture) =
                            textures.state(&resources.0, &mut images, &mut layouts, rsi, state)
                        else {
                            continue;
                        };
                        let start = if layer.animation_restart == 0 {
                            0.0
                        } else {
                            let now = clock.elapsed_secs();
                            let entry = views
                                .restarts
                                .entry((entity, layer.index))
                                .or_insert((layer.animation_restart, now));
                            if entry.0 != layer.animation_restart {
                                *entry = (layer.animation_restart, now);
                            }
                            entry.1
                        };
                        let sprite = Sprite {
                            image: texture.image.clone(),
                            texture_atlas: Some(TextureAtlas {
                                layout: texture.layout.clone(),
                                index: 0,
                            }),
                            color,
                            custom_size: Some(texture.size * layer.scale),
                            ..default()
                        };
                        let place = Transform::from_xyz(layer.offset.x, layer.offset.y, order);
                        let layer_view = LayerView {
                            texture,
                            rotation: layer.rotation,
                            offset: layer.offset,
                            order,
                            direction: layer.direction,
                            start,
                        };
                        if let Some(twin) = twin {
                            commands.spawn((
                                ChildOf(view),
                                Sprite {
                                    color: twin,
                                    ..sprite.clone()
                                },
                                place,
                                layer_view.clone(),
                                RenderLayers::layer(UNSHADED_LAYER),
                            ));
                        }
                        commands.spawn((ChildOf(view), sprite, place, layer_view));
                    }
                    LayerSource::Texture(path) => {
                        let Some((image, size)) = textures.single(&resources.0, &mut images, path)
                        else {
                            continue;
                        };
                        let sprite = Sprite {
                            image,
                            color,
                            custom_size: Some(size * layer.scale),
                            ..default()
                        };
                        let place = Transform::from_xyz(layer.offset.x, layer.offset.y, order)
                            .with_rotation(Quat::from_rotation_z(layer.rotation.0));
                        if let Some(twin) = twin {
                            commands.spawn((
                                ChildOf(view),
                                Sprite {
                                    color: twin,
                                    ..sprite.clone()
                                },
                                place,
                                RenderLayers::layer(UNSHADED_LAYER),
                            ));
                        }
                        commands.spawn((ChildOf(view), sprite, place));
                    }
                }
            }
        }
    }
    // Сущности, которых больше не видно.
    views
        .restarts
        .retain(|(entity, _), _| alive.contains(entity));
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
        let direction = match layer.direction {
            Some(fixed) => usize::from(fixed).min(directions - 1),
            None => rsi_direction(directions, facing),
        };
        let frame = layer.texture.state.frame_at(direction, now - layer.start);
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
    png_pixels(resources, sprite)
}

/// Пиксели картинки PNG: RGBA и ширина.
fn png_pixels(resources: &Resources, path: &str) -> Option<(Vec<u8>, u32)> {
    let bytes = resources.read(path.trim_start_matches('/')).ok()?;
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

/// Накладывает картинку размером в клетку на клетку `(tx, ty)` картинки чанка с учётом
/// прозрачности.
fn blend_tile(
    pixels: &mut [u8],
    side: u32,
    tx: u32,
    ty: u32,
    source: &[u8],
    width: u32,
    tile_px: u32,
) {
    for row in 0..tile_px {
        let dest_row = (CHUNK_SIZE as u32 - 1 - ty) * tile_px + row;
        for column in 0..tile_px.min(width) {
            let from = ((row * width + column) * 4) as usize;
            let to = ((dest_row * side + tx * tile_px + column) * 4) as usize;
            if from + 4 > source.len() || to + 4 > pixels.len() {
                continue;
            }
            let alpha = u32::from(source[from + 3]);
            for channel in 0..3 {
                let src = u32::from(source[from + channel]);
                let dst = u32::from(pixels[to + channel]);
                pixels[to + channel] = ((src * alpha + dst * (255 - alpha)) / 255) as u8;
            }
            let dst_alpha = u32::from(pixels[to + 3]);
            pixels[to + 3] = (alpha + dst_alpha * (255 - alpha) / 255) as u8;
        }
    }
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
    let any_edges = definitions.any_edges();
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
            // Края зависят от соседних чанков: картинка обновляется и при их изменении.
            let modified = if any_edges {
                (-1..=1)
                    .flat_map(|dx| (-1..=1).map(move |dy| (dx, dy)))
                    .filter_map(|(dx, dy)| {
                        tiles.chunk(ChunkPos {
                            x: position.x + dx,
                            y: position.y + dy,
                            z: position.z,
                        })
                    })
                    .map(|c| c.last_modified)
                    .max()
                    .unwrap_or(chunk.last_modified)
            } else {
                chunk.last_modified
            };
            if let Some((view, drawn)) = views.chunks.get(&(grid, position)).copied()
                && drawn == modified
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
            // Края соседних тайлов поверх клеток чанка, с прозрачностью.
            if any_edges {
                for index in 0..chunk.tiles().len() {
                    let coord: TileCoord = position.tile(index);
                    for (neighbor, direction) in definitions.edges_onto(tiles, coord) {
                        let Some(path) = definitions
                            .get(neighbor)
                            .and_then(|d| d.edges[direction].clone())
                        else {
                            continue;
                        };
                        let source = views
                            .edges
                            .entry(path.clone())
                            .or_insert_with(|| png_pixels(&resources.0, &path));
                        let Some((source, width)) = source else {
                            continue;
                        };
                        let (tx, ty) = (
                            coord.x.rem_euclid(CHUNK_SIZE) as u32,
                            coord.y.rem_euclid(CHUNK_SIZE) as u32,
                        );
                        blend_tile(&mut pixels, side, tx, ty, source, *width, tile_px);
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
            views.chunks.insert((grid, position), (view, modified));
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

/// Название сущности для надписей интерфейса: на языке клиента.
pub fn entity_label(world: &bevy_ecs::world::World, entity: bevy_ecs::entity::Entity) -> String {
    world.entity_name(entity)
}
