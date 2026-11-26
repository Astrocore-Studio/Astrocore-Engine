//! Проигрывание звуков клиента: очередь звуков мира игры → устройство вывода, громкость по
//! расстоянию до глаза, фоновые звуки ближайших источников. Образец — `AudioSystem`
//! Robust и `AmbientSoundSystem` SS14.

use std::collections::HashMap;
use std::time::Duration;

use astrocore_audio::{
    AmbientSound, AudioParams, AudioWorldExt, QueuedSound, SoundQueue, SoundSource, volume_to_gain,
};
use astrocore_map::{Transform as GameTransform, TransformWorldExt};
use astrocore_net::Detached;
use bevy::audio::{AudioSinkPlayback, PlaybackMode, Volume};
use bevy::prelude::*;

use crate::Game;
use crate::view::{Eye, GameResources};

/// Общая громкость клиента: множитель от 0 до 1.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MasterVolume(pub f32);

impl Default for MasterVolume {
    fn default() -> Self {
        Self(0.5)
    }
}

/// Сколько фоновых звуков играет одновременно (`ambience.max_sounds` SS14).
const MAX_AMBIENT_SOUNDS: usize = 16;

/// Как часто пересматривается набор фоновых звуков, секунды.
const AMBIENT_PERIOD: f32 = 0.25;

/// Загруженные звуки: путь ресурса → звук; `None` — файл не прочитался.
#[derive(Resource, Default)]
pub(crate) struct Sounds(HashMap<String, Option<Handle<AudioSource>>>);

impl Sounds {
    fn load(
        &mut self,
        resources: &GameResources,
        assets: &mut Assets<AudioSource>,
        path: &str,
    ) -> Option<Handle<AudioSource>> {
        self.0
            .entry(path.to_owned())
            .or_insert_with(|| match resources.0.read(path) {
                Ok(bytes) => Some(assets.add(AudioSource {
                    bytes: bytes.into(),
                })),
                Err(error) => {
                    warn!("звук {path}: {error}");
                    None
                }
            })
            .clone()
    }
}

/// Играющий звук: откуда он слышен.
#[derive(Component)]
pub(crate) struct Emitter {
    source: SoundSource<bevy_ecs::entity::Entity>,
    params: AudioParams,
}

/// Фоновые звуки, которые сейчас играют: сущность игры → проигрыватель.
#[derive(Resource, Default)]
pub(crate) struct AmbientPlaying(HashMap<bevy_ecs::entity::Entity, Entity>);

/// Множитель громкости звука для глаза.
fn gain(
    world: &bevy_ecs::world::World,
    eye: &Eye,
    source: SoundSource<bevy_ecs::entity::Entity>,
    params: &AudioParams,
) -> f32 {
    let (position, level) = match source {
        SoundSource::Global => return volume_to_gain(params.volume),
        SoundSource::Position { position, z } => (position, z),
        SoundSource::Entity(entity) => {
            let on_eye_map = world
                .get::<GameTransform>(entity)
                .is_some_and(|transform| transform.map() == eye.map && eye.map.is_some());
            match world.world_transform(entity) {
                Some(placement) if on_eye_map => (placement.position, placement.level),
                _ => return 0.0,
            }
        }
    };
    if level != eye.level {
        return 0.0;
    }
    let listener = astrocore_math::Vec2::new(eye.position.x, eye.position.y);
    params.gain_at(position.distance(listener))
}

fn spawn_player(
    commands: &mut Commands,
    handle: Handle<AudioSource>,
    sound: &QueuedSound,
    volume: f32,
) -> Entity {
    commands
        .spawn((
            AudioPlayer::new(handle),
            PlaybackSettings {
                mode: if sound.params.looped {
                    PlaybackMode::Loop
                } else {
                    PlaybackMode::Despawn
                },
                volume: Volume::Linear(volume),
                speed: sound.params.pitch.max(0.01),
                start_position: (sound.params.play_offset_seconds > 0.0)
                    .then(|| Duration::from_secs_f32(sound.params.play_offset_seconds)),
                ..default()
            },
            Emitter {
                source: sound.source,
                params: sound.params,
            },
        ))
        .id()
}

/// Забирает звуки из очереди мира игры и запускает их.
pub(crate) fn play_queued(
    mut commands: Commands,
    mut game: ResMut<Game>,
    resources: Res<GameResources>,
    eye: Res<Eye>,
    master: Res<MasterVolume>,
    mut sounds: ResMut<Sounds>,
    mut assets: ResMut<Assets<AudioSource>>,
) {
    let queued = match game.0.get_resource_mut::<SoundQueue>() {
        Some(mut queue) if !queue.0.is_empty() => std::mem::take(&mut queue.0),
        _ => return,
    };
    for sound in queued {
        let volume = gain(&game.0, &eye, sound.source, &sound.params) * master.0;
        // Разовый звук, которого не слышно в момент начала, не запускается.
        if volume <= 0.0 && !sound.params.looped {
            continue;
        }
        if let Some(handle) = sounds.load(&resources, &mut assets, &sound.path) {
            spawn_player(&mut commands, handle, &sound, volume);
        }
    }
}

/// Громкость играющих звуков следует за расстоянием до глаза.
pub(crate) fn update_emitters(
    game: Res<Game>,
    eye: Res<Eye>,
    master: Res<MasterVolume>,
    mut playing: Query<(&Emitter, &mut AudioSink)>,
) {
    for (emitter, mut sink) in &mut playing {
        let volume = gain(&game.0, &eye, emitter.source, &emitter.params) * master.0;
        sink.set_volume(Volume::Linear(volume));
    }
}

/// Запускает фоновые звуки ближайших источников и останавливает ушедшие из слышимости.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_ambient(
    mut commands: Commands,
    mut game: ResMut<Game>,
    resources: Res<GameResources>,
    eye: Res<Eye>,
    time: Res<Time>,
    mut wait: Local<f32>,
    mut playing: ResMut<AmbientPlaying>,
    mut sounds: ResMut<Sounds>,
    mut assets: ResMut<Assets<AudioSource>>,
) {
    *wait -= time.delta_secs();
    if *wait > 0.0 {
        return;
    }
    *wait = AMBIENT_PERIOD;
    let world = &mut game.0;
    let listener = astrocore_math::Vec2::new(eye.position.x, eye.position.y);
    let mut near: Vec<(f32, bevy_ecs::entity::Entity)> = Vec::new();
    if eye.map.is_some() {
        let mut sources = world.query_filtered::<(
            bevy_ecs::entity::Entity,
            &AmbientSound,
            &GameTransform,
        ), Without<Detached>>();
        for (entity, ambient, transform) in sources.iter(world) {
            if !ambient.enabled || transform.map() != eye.map {
                continue;
            }
            let Some(placement) = world.world_transform(entity) else {
                continue;
            };
            let distance = placement.position.distance(listener);
            if placement.level == eye.level && distance <= ambient.range {
                near.push((distance, entity));
            }
        }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(MAX_AMBIENT_SOUNDS);

    playing.0.retain(|entity, player| {
        let keep = near.iter().any(|(_, wanted)| wanted == entity);
        if !keep {
            commands.entity(*player).despawn();
        }
        keep
    });
    for (_, entity) in near {
        if playing.0.contains_key(&entity) {
            continue;
        }
        let Some(ambient) = world.get::<AmbientSound>(entity).cloned() else {
            continue;
        };
        let Some((path, params)) = world.resolve_sound(&ambient.sound) else {
            continue;
        };
        let Some(handle) = sounds.load(&resources, &mut assets, &path) else {
            continue;
        };
        let sound = QueuedSound {
            path,
            params: AudioParams {
                volume: params.volume + ambient.volume,
                max_distance: ambient.range,
                looped: true,
                ..params
            },
            source: SoundSource::Entity(entity),
        };
        // Громкость выставит `update_emitters` в этом же кадре.
        playing
            .0
            .insert(entity, spawn_player(&mut commands, handle, &sound, 0.0));
    }
}
