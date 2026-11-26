//! Звук Astrocore по образцу `Robust.Shared/Audio`: описание звука в данных
//! ([`SoundSpecifier`]), рассылка звуков клиентам и фоновые источники.
//!
//! Крейт не знает об устройстве вывода. Сервер решает, кто слышит звук, и отправляет
//! клиентам сообщение; клиент складывает звуки в очередь [`SoundQueue`], а проигрывает их
//! слой окна (`astrocore-client`).

use astrocore_ecs::{GamePlugin, GameRng, RegistryWorldExt};
use astrocore_map::{MapCoordinates, MapWorldExt, Transform, TransformWorldExt};
use astrocore_math::Vec2;
use astrocore_net::{
    NetEntities, NetEntity, NetEvent, NetEventWorldExt, NetRegistryWorldExt, NetTarget, Session,
};
use astrocore_prototypes::Prototypes;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Дальность звука по умолчанию в тайлах (`SharedAudioSystem.DefaultSoundRange`).
pub const DEFAULT_SOUND_RANGE: f32 = 15.0;

fn one() -> f32 {
    1.0
}

fn default_range() -> f32 {
    DEFAULT_SOUND_RANGE
}

/// Параметры проигрывания (`AudioParams` Robust).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioParams {
    /// Громкость в децибелах относительно записи.
    #[serde(default)]
    pub volume: f32,
    /// Множитель высоты и скорости.
    #[serde(default = "one")]
    pub pitch: f32,
    /// Дальше этого расстояния звук не слышен.
    #[serde(default = "default_range")]
    pub max_distance: f32,
    /// Скорость затухания с расстоянием.
    #[serde(default = "one")]
    pub rolloff_factor: f32,
    /// Расстояние, до которого звук не затухает.
    #[serde(default = "one")]
    pub reference_distance: f32,
    #[serde(default, rename = "loop")]
    pub looped: bool,
    #[serde(default)]
    pub play_offset_seconds: f32,
    /// Случайный разброс высоты: доля от высоты.
    #[serde(default)]
    pub variation: Option<f32>,
}

impl Default for AudioParams {
    fn default() -> Self {
        Self {
            volume: 0.0,
            pitch: 1.0,
            max_distance: DEFAULT_SOUND_RANGE,
            rolloff_factor: 1.0,
            reference_distance: 1.0,
            looped: false,
            play_offset_seconds: 0.0,
            variation: None,
        }
    }
}

/// Децибелы → множитель громкости (`SharedAudioSystem.VolumeToGain`).
pub fn volume_to_gain(volume: f32) -> f32 {
    10f32.powf(volume / 10.0).max(0.0)
}

impl AudioParams {
    /// Множитель громкости на расстоянии от слушателя: громкость записи и затухание
    /// (обратное расстояние с ограничением, как в OpenAL). Дальше `max_distance` — тишина.
    pub fn gain_at(&self, distance: f32) -> f32 {
        if distance > self.max_distance {
            return 0.0;
        }
        let reference = self.reference_distance.max(f32::EPSILON);
        let far = distance.max(reference);
        let falloff = reference / (reference + self.rolloff_factor * (far - reference));
        volume_to_gain(self.volume) * falloff
    }
}

/// Звук в данных: файл или коллекция файлов. В YAML — путь строкой, `path: …` или
/// `collection: …`, рядом — `params`.
#[derive(Debug, Clone, PartialEq)]
pub enum SoundSpecifier {
    Path {
        path: String,
        params: AudioParams,
    },
    Collection {
        collection: String,
        params: AudioParams,
    },
}

impl SoundSpecifier {
    pub fn path(path: &str) -> Self {
        Self::Path {
            path: path.to_owned(),
            params: AudioParams::default(),
        }
    }

    pub fn collection(collection: &str) -> Self {
        Self::Collection {
            collection: collection.to_owned(),
            params: AudioParams::default(),
        }
    }

    pub fn params(&self) -> AudioParams {
        match self {
            Self::Path { params, .. } | Self::Collection { params, .. } => *params,
        }
    }

    pub fn with_params(mut self, new: AudioParams) -> Self {
        match &mut self {
            Self::Path { params, .. } | Self::Collection { params, .. } => *params = new,
        }
        self
    }
}

/// Запись для двоичных форматов (сеть).
#[derive(Serialize, Deserialize)]
struct Wire {
    collection: bool,
    name: String,
    params: AudioParams,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    Path(String),
    Map {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        params: AudioParams,
    },
}

impl Serialize for SoundSpecifier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let (collection, name, params) = match self {
            Self::Path { path, params } => (false, path, params),
            Self::Collection { collection, params } => (true, collection, params),
        };
        if !serializer.is_human_readable() {
            return Wire {
                collection,
                name: name.clone(),
                params: *params,
            }
            .serialize(serializer);
        }
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(if collection { "collection" } else { "path" }, name)?;
        map.serialize_entry("params", params)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for SoundSpecifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        if !deserializer.is_human_readable() {
            let wire = Wire::deserialize(deserializer)?;
            return Ok(if wire.collection {
                Self::Collection {
                    collection: wire.name,
                    params: wire.params,
                }
            } else {
                Self::Path {
                    path: wire.name,
                    params: wire.params,
                }
            });
        }
        match Text::deserialize(deserializer)? {
            Text::Path(path) => Ok(Self::path(&path)),
            Text::Map {
                path: Some(path),
                collection: None,
                params,
            } => Ok(Self::Path { path, params }),
            Text::Map {
                path: None,
                collection: Some(collection),
                params,
            } => Ok(Self::Collection { collection, params }),
            Text::Map { .. } => Err(D::Error::custom(
                "звук задаётся одним из полей `path` или `collection`",
            )),
        }
    }
}

/// Откуда слышен звук.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SoundSource<E> {
    /// Отовсюду с одной громкостью.
    Global,
    /// От сущности: звук движется вместе с ней.
    Entity(E),
    /// Из точки карты слушателя на этаже `z`.
    Position { position: Vec2, z: i16 },
}

/// Сообщение сервера клиенту: проиграть звук.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaySound {
    /// Путь файла от каталога ресурсов.
    pub path: String,
    pub params: AudioParams,
    pub source: SoundSource<NetEntity>,
}

/// Звук в очереди клиента.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedSound {
    pub path: String,
    pub params: AudioParams,
    pub source: SoundSource<Entity>,
}

/// Звуки, которые клиенту пора проиграть: слой окна забирает их каждый кадр. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct SoundQueue(pub Vec<QueuedSound>);

/// Фоновый звук сущности: гул генератора, шум вентиляции. Клиент проигрывает его по кругу,
/// пока слушатель ближе `range`. В данных — компонент `AmbientSound`
/// (`AmbientSoundComponent` SS14).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmbientSound {
    #[serde(default = "yes")]
    pub enabled: bool,
    pub sound: SoundSpecifier,
    #[serde(default = "ambient_range")]
    pub range: f32,
    #[serde(default = "ambient_volume")]
    pub volume: f32,
}

fn yes() -> bool {
    true
}

fn ambient_range() -> f32 {
    2.0
}

fn ambient_volume() -> f32 {
    -10.0
}

/// Путь ресурса: пути звуков в данных начинаются с `/`.
fn resource_path(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

/// Звук на `World`.
pub trait AudioWorldExt {
    /// Выбирает файл звука и высоту: файл коллекции и разброс высоты — случайные.
    fn resolve_sound(&mut self, sound: &SoundSpecifier) -> Option<(String, AudioParams)>;

    /// Сервер: проиграть звук без положения.
    fn play_global(&mut self, sound: &SoundSpecifier, target: NetTarget);

    /// Сервер: проиграть звук от сущности всем, кто в пределах слышимости. `except` —
    /// сущность игрока, который уже проиграл звук сам (предсказание).
    fn play_entity(&mut self, sound: &SoundSpecifier, source: Entity, except: Option<Entity>);

    /// Сервер: проиграть звук из точки карты.
    fn play_at(&mut self, sound: &SoundSpecifier, at: MapCoordinates, except: Option<Entity>);

    /// Клиент: проиграть звук у себя.
    fn play_local(&mut self, sound: &SoundSpecifier, source: SoundSource<Entity>);
}

/// Сессии, чей игрок слышит звук из точки карты.
fn listeners(
    world: &mut World,
    at: MapCoordinates,
    range: f32,
    except: Option<Entity>,
) -> Vec<Entity> {
    let Some(map) = world.map_entity(at.map) else {
        return Vec::new();
    };
    let sessions: Vec<(Entity, Entity)> = world
        .query::<(Entity, &Session)>()
        .iter(world)
        .filter_map(|(session, data)| Some((session, data.attached?)))
        .collect();
    sessions
        .into_iter()
        .filter(|(_, player)| Some(*player) != except)
        .filter(|(_, player)| {
            world.get::<Transform>(*player).and_then(Transform::map) == Some(map)
                && world.world_transform(*player).is_some_and(|placement| {
                    placement.level == at.z && placement.position.distance(at.position) <= range
                })
        })
        .map(|(session, _)| session)
        .collect()
}

impl AudioWorldExt for World {
    fn resolve_sound(&mut self, sound: &SoundSpecifier) -> Option<(String, AudioParams)> {
        let (path, mut params) = match sound {
            SoundSpecifier::Path { path, params } => (resource_path(path), *params),
            SoundSpecifier::Collection { collection, params } => {
                let files: Vec<String> = self
                    .get_resource::<Prototypes>()?
                    .resolved_node("soundCollection", collection)?
                    .get("files")?
                    .as_seq()?
                    .iter()
                    .filter_map(|file| file.as_str().map(resource_path))
                    .collect();
                let file = self.resource_mut::<GameRng>().pick(&files)?.clone();
                (file, *params)
            }
        };
        if let Some(variation) = params.variation.filter(|v| *v != 0.0) {
            let spread = self
                .resource_mut::<GameRng>()
                .range_f32(-variation, variation);
            params.pitch *= 1.0 + spread;
        }
        Some((path, params))
    }

    fn play_global(&mut self, sound: &SoundSpecifier, target: NetTarget) {
        if let Some((path, params)) = self.resolve_sound(sound) {
            let event = PlaySound {
                path,
                params,
                source: SoundSource::Global,
            };
            self.send_net_event(target, &event);
        }
    }

    fn play_entity(&mut self, sound: &SoundSpecifier, source: Entity, except: Option<Entity>) {
        let (Some(at), Some(net)) = (
            self.map_coordinates(source),
            self.get::<NetEntity>(source).copied(),
        ) else {
            return;
        };
        let Some((path, params)) = self.resolve_sound(sound) else {
            return;
        };
        let event = PlaySound {
            path,
            params,
            source: SoundSource::Entity(net),
        };
        for session in listeners(self, at, params.max_distance, except) {
            self.send_net_event(NetTarget::Session(session), &event);
        }
    }

    fn play_at(&mut self, sound: &SoundSpecifier, at: MapCoordinates, except: Option<Entity>) {
        let Some((path, params)) = self.resolve_sound(sound) else {
            return;
        };
        let event = PlaySound {
            path,
            params,
            source: SoundSource::Position {
                position: at.position,
                z: at.z,
            },
        };
        for session in listeners(self, at, params.max_distance, except) {
            self.send_net_event(NetTarget::Session(session), &event);
        }
    }

    fn play_local(&mut self, sound: &SoundSpecifier, source: SoundSource<Entity>) {
        if let Some((path, params)) = self.resolve_sound(sound) {
            self.get_resource_or_insert_with(SoundQueue::default)
                .0
                .push(QueuedSound {
                    path,
                    params,
                    source,
                });
        }
    }
}

fn receive(world: &mut World, event: &mut NetEvent<PlaySound>) {
    // Звуки приходят только от сервера.
    if event.session.is_some() {
        return;
    }
    let source = match event.event.source {
        SoundSource::Global => SoundSource::Global,
        SoundSource::Position { position, z } => SoundSource::Position { position, z },
        SoundSource::Entity(net) => {
            match world
                .get_resource::<NetEntities>()
                .and_then(|all| all.entity(net))
            {
                Some(entity) => SoundSource::Entity(entity),
                // Источник вне зоны видимости клиента: звук не слышен.
                None => return,
            }
        }
    };
    world
        .get_resource_or_insert_with(SoundQueue::default)
        .0
        .push(QueuedSound {
            path: event.event.path.clone(),
            params: event.event.params,
            source,
        });
}

/// Звук: одинаково подключается на сервере и клиенте.
pub struct AudioPlugin;

impl GamePlugin for AudioPlugin {
    fn build(&self, world: &mut World) {
        world.init_resource::<SoundQueue>();
        world.register_data_component::<AmbientSound>();
        world.register_net_serde::<AmbientSound>();
        world.register_net_event::<PlaySound>();
        world.subscribe_net_event::<PlaySound>(receive);
    }
}

#[cfg(test)]
mod tests;
