//! Движение персонажей: ввод игрока, расчёт скорости и предсказание на клиенте.
//!
//! Расчёт скорости — `SharedMoverController.HandleMobMovement` из
//! `Content.Shared/Movement/Systems` SS14 @ 2f4a2aad: трение, затем разгон к желаемой
//! скорости. Без гравитации (`GravityAffected.weightless`, его считает игра) — свои трение и
//! разгон; разгоняться можно, только касаясь грида или твёрдой стены рядом. Передача
//! управления и звуки шагов — задачи игры.
//!
//! Клиент каждый такт отправляет серверу кадр ввода с номером и сразу считает его у себя.
//! Сервер обрабатывает по кадру за такт и сообщает номер последнего обработанного. Получив
//! состояние, клиент возвращает своего персонажа к положению с сервера и заново считает
//! кадры, которые сервер ещё не обработал. Расчёт у обоих одинаков, поэтому персонаж
//! отзывается на ввод сразу и не дёргается при любой задержке сети.

use std::collections::{HashMap, VecDeque};

use astrocore_console::{CVarDef, CVarFlags, ConfigWorldExt};
use astrocore_ecs::{
    EventWorldExt, GamePlugin, GameTime, GameWorldExt, Paused, PreUpdate, RegistryWorldExt, Update,
};
use astrocore_map::{Map, MapWorldExt, Placement, Transform, TransformWorldExt};
use astrocore_math::{Angle, Box2, Vec2};
use astrocore_net::{
    ClientNet, NetEvent, NetEventWorldExt, NetRegistryWorldExt, PlayerDisconnected, Session,
    StateApplied, StateApplying,
};
use astrocore_physics::{BodyType, Fixtures, Physics, PhysicsWorldExt, Pose, step_physics};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::Has;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

/// Множитель трения о пол (`physics.tile_friction` SS14).
pub const TILE_FRICTION: CVarDef<f32> = CVarDef::new(
    "physics.tile_friction",
    8.0,
    CVarFlags::SERVER
        .union(CVarFlags::REPLICATED)
        .union(CVarFlags::ARCHIVE),
    "Множитель трения персонажей и предметов о пол.",
);

const FLAGS: CVarFlags = CVarFlags::SERVER
    .union(CVarFlags::REPLICATED)
    .union(CVarFlags::ARCHIVE);

/// Наименьшее затухание (`physics.min_friction`).
pub const MIN_FRICTION: CVarDef<f32> = CVarDef::new(
    "physics.min_friction",
    0.0,
    FLAGS,
    "Наименьшее трение тела.",
);
/// Трение в воздухе и в невесомости на гриде (`physics.air_friction`).
pub const AIR_FRICTION: CVarDef<f32> =
    CVarDef::new("physics.air_friction", 0.2, FLAGS, "Трение тела в воздухе.");
/// Трение в космосе вне гридов (`physics.offgrid_friction`).
pub const OFFGRID_FRICTION: CVarDef<f32> = CVarDef::new(
    "physics.offgrid_friction",
    0.05,
    FLAGS,
    "Трение тела в космосе вне гридов.",
);

/// Сущность чувствует гравитацию (`GravityAffectedComponent`): игра пишет, невесома ли она.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GravityAffected {
    pub weightless: bool,
    /// Невесомость от грида или карты, а не от снаряжения.
    pub grid_weightless_status: bool,
}

impl Default for GravityAffected {
    fn default() -> Self {
        Self {
            weightless: true,
            grid_weightless_status: false,
        }
    }
}

/// Существо отталкивается от стен рядом (`MobMoverComponent`).
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MobMover {
    /// На каком расстоянии от тела можно оттолкнуться.
    pub grab_range: f32,
}

impl Default for MobMover {
    fn default() -> Self {
        Self { grab_range: 1.0 }
    }
}

/// В невесомости всегда есть опора (`MovementAlwaysTouchingComponent`).
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovementAlwaysTouching {}

/// Удерживаемые клавиши движения (`MoveButtons` SS14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MoveButtons(pub u8);

impl MoveButtons {
    pub const NONE: MoveButtons = MoveButtons(0);
    pub const UP: MoveButtons = MoveButtons(1);
    pub const DOWN: MoveButtons = MoveButtons(2);
    pub const LEFT: MoveButtons = MoveButtons(4);
    pub const RIGHT: MoveButtons = MoveButtons(8);
    /// Шаг вместо бега.
    pub const WALK: MoveButtons = MoveButtons(16);

    pub const fn contains(self, other: MoveButtons) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: MoveButtons) -> MoveButtons {
        MoveButtons(self.0 | other.0)
    }

    /// Кнопки `self` без кнопок `other`.
    pub const fn difference(self, other: MoveButtons) -> MoveButtons {
        MoveButtons(self.0 & !other.0)
    }

    /// Направление движения: единичный вектор или ноль. Противоположные клавиши гасят
    /// друг друга.
    pub fn direction(self) -> Vec2 {
        let axis = |positive: MoveButtons, negative: MoveButtons| {
            f32::from(u8::from(self.contains(positive)))
                - f32::from(u8::from(self.contains(negative)))
        };
        Vec2::new(
            axis(MoveButtons::RIGHT, MoveButtons::LEFT),
            axis(MoveButtons::UP, MoveButtons::DOWN),
        )
        .normalize_or_zero()
    }
}

fn yes() -> bool {
    true
}

/// Сущность, которой управляет ввод. В данных — компонент `InputMover`.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMover {
    /// Удерживаемые клавиши. По сети не передаются: чужой ввод клиенту не нужен.
    #[serde(skip)]
    pub held: MoveButtons,
    /// Может ли сущность двигаться сама (оглушение, наручники и подобное выключают).
    #[serde(default = "yes")]
    pub can_move: bool,
}

impl Default for InputMover {
    fn default() -> Self {
        Self {
            held: MoveButtons::NONE,
            can_move: true,
        }
    }
}

/// Скорости движения. В данных — компонент `MovementSpeedModifier`; значения по умолчанию —
/// как в SS14.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MovementSpeedModifier {
    pub base_walk_speed: f32,
    pub base_sprint_speed: f32,
    pub base_acceleration: f32,
    pub base_friction: f32,
    /// Ниже этой скорости трение не действует.
    pub minimum_friction_speed: f32,
    /// Множители скорости от снаряжения, ранений и эффектов.
    pub walk_speed_modifier: f32,
    pub sprint_speed_modifier: f32,
    /// Невесомость: множитель трения, доля скорости ходьбы и разгон.
    pub base_weightless_friction: f32,
    pub base_weightless_modifier: f32,
    pub base_weightless_acceleration: f32,
}

impl Default for MovementSpeedModifier {
    fn default() -> Self {
        Self {
            base_walk_speed: 2.5,
            base_sprint_speed: 4.5,
            base_acceleration: 20.0,
            base_friction: 2.5,
            minimum_friction_speed: 0.005,
            walk_speed_modifier: 1.0,
            sprint_speed_modifier: 1.0,
            base_weightless_friction: 1.0,
            base_weightless_modifier: 0.7,
            base_weightless_acceleration: 1.0,
        }
    }
}

/// Сущность не поворачивается по направлению движения (`NoRotateOnMove` SS14).
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct NoRotateOnMove;

/// Есть ли рядом твёрдая закреплённая опора (`IsAroundCollider`): статичное тело, с которым
/// сталкивается тело сущности, ближе `grab_range` от его краёв.
fn is_around_collider(world: &World, entity: Entity, grab_range: f32) -> bool {
    let Some(at) = world.map_coordinates(entity) else {
        return false;
    };
    let fixtures = world.get::<Fixtures>(entity);
    let mask = fixtures.map_or(0, |f| f.fixtures.values().fold(0, |m, x| m | x.mask.0));
    let pose = Pose {
        position: Vec2::ZERO,
        rotation: Angle(0.0),
    };
    let half = fixtures
        .and_then(|f| {
            f.fixtures
                .values()
                .map(|x| x.shape.at(&pose).aabb())
                .reduce(|a, b| a.union(&b))
        })
        .map_or(Vec2::splat(0.35), |b| b.size() * 0.5);
    let size = (half + Vec2::splat(grab_range)) * 2.0;
    let Some(map) = world.map_entity(at.map) else {
        return false;
    };
    let mut roots: Vec<(Entity, Vec2, i16)> = vec![(map, at.position, at.z)];
    if let Some(grids) = world.get::<Map>(map).map(|m| m.grids().to_vec()) {
        for grid in grids {
            if let Some(placement) = world.world_transform(grid) {
                roots.push((
                    grid,
                    placement.to_local(at.position),
                    at.z - placement.level,
                ));
            }
        }
    }
    roots.into_iter().any(|(root, local, z)| {
        world
            .entities_intersecting(root, z, Box2::centered(local, size))
            .into_iter()
            .any(|other| {
                if other == entity {
                    return false;
                }
                let Some(body) = world.get::<Physics>(other) else {
                    return false;
                };
                let layer = world
                    .get::<Fixtures>(other)
                    .map_or(0, |f| f.fixtures.values().fold(0, |l, x| l | x.layer.0));
                body.body_type == BodyType::Static && body.can_collide && mask & layer != 0
            })
    })
}

/// Один такт движения сущности: скорость по удерживаемым клавишам и поворот по ходу.
pub fn mover_step(world: &mut World, entity: Entity, dt: f32) {
    let Some(mover) = world.get::<InputMover>(entity) else {
        return;
    };
    let Some(body) = world.get::<Physics>(entity) else {
        return;
    };
    if !mover.can_move {
        return;
    }
    let held = mover.held;
    let mut velocity = body.linear_velocity;
    let speeds = world
        .get::<MovementSpeedModifier>(entity)
        .cloned()
        .unwrap_or_default();
    let weightless = world
        .get::<GravityAffected>(entity)
        .is_some_and(|g| g.weightless)
        && !matches!(body.body_type, BodyType::Static | BodyType::Kinematic);
    let walking = held.contains(MoveButtons::WALK);

    let (wish, acceleration, mut friction, touching) = if weightless {
        let speed = speeds.base_weightless_modifier
            * if walking {
                speeds.base_walk_speed
            } else {
                speeds.base_sprint_speed
            };
        let touching = world.get::<MovementAlwaysTouching>(entity).is_some()
            || world
                .get::<Transform>(entity)
                .is_some_and(|t| t.grid().is_some())
            || world
                .get::<MobMover>(entity)
                .is_some_and(|m| is_around_collider(world, entity, m.grab_range));
        let damping = if touching {
            world.cvar(&AIR_FRICTION)
        } else {
            world.cvar(&OFFGRID_FRICTION)
        };
        (
            held.direction() * speed,
            speeds.base_weightless_acceleration,
            damping * speeds.base_weightless_friction,
            touching,
        )
    } else {
        let speed = if walking {
            speeds.base_walk_speed * speeds.walk_speed_modifier
        } else {
            speeds.base_sprint_speed * speeds.sprint_speed_modifier
        };
        (
            held.direction() * speed,
            speeds.base_acceleration,
            speeds.base_friction * world.cvar(&TILE_FRICTION),
            true,
        )
    };
    if wish != Vec2::ZERO {
        friction = friction.min(acceleration);
    }
    friction = friction.max(world.cvar(&MIN_FRICTION));

    // Трение.
    if velocity.length() >= speeds.minimum_friction_speed {
        velocity *= (1.0 - dt * friction).clamp(0.0, 1.0);
    }
    // Разгон к желаемой скорости: в невесомости — только от опоры.
    let wish_speed = wish.length();
    if wish_speed > 0.0 && (!weightless || touching) {
        let direction = wish / wish_speed;
        let missing = wish_speed - velocity.dot(direction);
        if missing > 0.0 {
            velocity += direction * (acceleration * dt * wish_speed).min(missing);
        }
    }
    world.set_linear_velocity(entity, velocity);

    if wish != Vec2::ZERO && world.get::<NoRotateOnMove>(entity).is_none() {
        let _ = world.set_local_rotation(entity, Angle::from_world_vec(wish));
    }
}

/// Кадр ввода клиента: номер и удерживаемые клавиши. Клиент → сервер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveInput {
    pub sequence: u32,
    pub buttons: MoveButtons,
}

/// Сколько кадров ввода сервер держит в очереди сессии: остальные обрабатываются сразу,
/// чтобы отставший клиент догнал.
const MAX_QUEUED_FRAMES: usize = 4;

/// Очереди кадров ввода по сессиям (сервер). Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct MoveQueues {
    queues: HashMap<Entity, VecDeque<MoveInput>>,
}

/// Сервер: применяет по кадру ввода за такт к сущности каждой сессии. Этап `PreUpdate`.
fn apply_queued_input(world: &mut World) {
    let sessions: Vec<Entity> = world
        .resource::<MoveQueues>()
        .queues
        .iter()
        .filter(|(_, queue)| !queue.is_empty())
        .map(|(session, _)| *session)
        .collect();
    for session in sessions {
        let mut queues = world.resource_mut::<MoveQueues>();
        let Some(queue) = queues.queues.get_mut(&session) else {
            continue;
        };
        // Клиент накопил кадры (всплеск задержки): лишние применяются разом.
        let mut frame = None;
        while queue.len() > MAX_QUEUED_FRAMES {
            frame = queue.pop_front();
        }
        let frame = queue.pop_front().or(frame);
        let Some(frame) = frame else {
            continue;
        };
        let Some(mut state) = world.get_mut::<Session>(session) else {
            continue;
        };
        state.last_input = frame.sequence;
        let Some(entity) = state.attached else {
            continue;
        };
        if let Some(mut mover) = world.get_mut::<InputMover>(entity)
            && mover.held != frame.buttons
        {
            mover.held = frame.buttons;
        }
    }
}

/// Сервер: двигает все управляемые сущности. Этап `Update`.
fn move_all(world: &mut World) {
    let dt = world.resource::<GameTime>().frame_time();
    let movers: Vec<Entity> = world
        .query::<(Entity, &InputMover, &Physics, Has<Paused>)>()
        .iter(world)
        // Стоящие без ввода сущности пропускаются: их большинство.
        .filter(|(_, mover, body, paused)| {
            (!paused || body.ignore_paused)
                && (mover.held != MoveButtons::NONE || body.linear_velocity != Vec2::ZERO)
        })
        .map(|(entity, ..)| entity)
        .collect();
    for entity in movers {
        mover_step(world, entity, dt);
    }
}

/// Ввод игрока на клиенте: клавиши, удерживаемые сейчас. Заполняет слой окна. Ресурс `World`.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct LocalInput {
    pub buttons: MoveButtons,
}

/// Положение и скорость по данным сервера.
#[derive(Debug, Clone, Copy)]
struct Authoritative {
    parent: Option<Entity>,
    position: Vec2,
    rotation: Angle,
    level: i16,
    velocity: Vec2,
}

/// Предсказание движения своего персонажа (клиент). Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct Prediction {
    next_sequence: u32,
    /// Кадры ввода, которые сервер ещё не обработал.
    pending: VecDeque<MoveInput>,
    /// Последнее состояние персонажа, пришедшее с сервера.
    authoritative: Option<(Entity, Authoritative)>,
    /// Пришло новое состояние: неподтверждённые кадры нужно пересчитать.
    replay: bool,
}

impl Prediction {
    /// Сколько кадров ввода ждут подтверждения сервера.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

fn capture(world: &World, entity: Entity) -> Option<Authoritative> {
    let transform = world.get::<Transform>(entity)?;
    Some(Authoritative {
        parent: world.parent(entity),
        position: transform.local_position(),
        rotation: transform.local_rotation(),
        level: transform.level(),
        velocity: world.get::<Physics>(entity)?.linear_velocity,
    })
}

fn restore(world: &mut World, entity: Entity, state: Authoritative) {
    let Some(transform) = world.get::<Transform>(entity) else {
        return;
    };
    let (anchored, no_rot) = (transform.anchored(), transform.no_rot());
    let _ = world.set_placement(
        entity,
        Placement {
            parent: state.parent,
            position: state.position,
            rotation: state.rotation,
            level: state.level,
            anchored,
            no_rot,
        },
    );
    if let Some(mut body) = world.get_mut::<Physics>(entity) {
        body.linear_velocity = state.velocity;
        body.awake = true;
    }
}

fn simulate(world: &mut World, entity: Entity, buttons: MoveButtons, dt: f32) {
    if let Some(mut mover) = world.get_mut::<InputMover>(entity) {
        mover.held = buttons;
    }
    mover_step(world, entity, dt);
    step_physics(world, dt, Some(entity));
}

/// Клиент: отправляет кадр ввода и считает движение своего персонажа. Этап `PreUpdate`,
/// после приёма состояний.
fn predict(world: &mut World) {
    let Some(entity) = world.resource::<ClientNet>().attached else {
        return;
    };
    if world.get::<InputMover>(entity).is_none() || world.get::<Physics>(entity).is_none() {
        return;
    }
    let dt = world.resource::<GameTime>().frame_time();
    let acked = world.resource::<ClientNet>().last_input;
    let mut prediction = world.resource_mut::<Prediction>();
    if std::mem::take(&mut prediction.replay) {
        while prediction
            .pending
            .front()
            .is_some_and(|frame| frame.sequence <= acked)
        {
            prediction.pending.pop_front();
        }
        let frames: Vec<MoveInput> = prediction.pending.iter().copied().collect();
        for frame in frames {
            simulate(world, entity, frame.buttons, dt);
        }
    }

    let buttons = world.resource::<LocalInput>().buttons;
    let mut prediction = world.resource_mut::<Prediction>();
    prediction.next_sequence += 1;
    let frame = MoveInput {
        sequence: prediction.next_sequence,
        buttons,
    };
    prediction.pending.push_back(frame);
    world.send_net_event_to_server(&frame);
    simulate(world, entity, buttons, dt);
}

fn register_shared(world: &mut World) {
    world.register_data_component::<InputMover>();
    world.register_data_component::<MovementSpeedModifier>();
    world.register_data_component::<NoRotateOnMove>();
    world.register_data_component::<GravityAffected>();
    world.register_data_component::<MobMover>();
    world.register_data_component::<MovementAlwaysTouching>();
    world.register_net_serde::<GravityAffected>();
    world.register_net_serde::<MovementAlwaysTouching>();
    world.register_cvar(&MIN_FRICTION);
    world.register_cvar(&AIR_FRICTION);
    world.register_cvar(&OFFGRID_FRICTION);
    world.register_net_serde::<InputMover>();
    world.register_net_serde::<MovementSpeedModifier>();
    world.register_net_event::<MoveInput>();
    world.register_cvar(&TILE_FRICTION);
}

/// Движение на сервере: очереди ввода сессий и расчёт скорости всех управляемых сущностей.
/// Подключается после сетевого плагина и физики.
pub struct MovementServerPlugin;

impl GamePlugin for MovementServerPlugin {
    fn build(&self, world: &mut World) {
        register_shared(world);
        world.init_resource::<MoveQueues>();
        world.subscribe_net_event::<MoveInput>(|world, event: &mut NetEvent<MoveInput>| {
            let Some(session) = event.session else {
                return;
            };
            let mut queues = world.resource_mut::<MoveQueues>();
            let queue = queues.queues.entry(session).or_default();
            // Кадры приходят по порядку; повтор или старый кадр отбрасывается.
            if queue
                .back()
                .is_none_or(|last| event.event.sequence > last.sequence)
            {
                queue.push_back(event.event);
            }
        });
        world.subscribe_broadcast::<PlayerDisconnected>(|world, event| {
            world
                .resource_mut::<MoveQueues>()
                .queues
                .remove(&event.session);
        });
        world.add_game_systems(
            PreUpdate,
            apply_queued_input.after(astrocore_net::server_receive),
        );
        world.add_game_systems(Update, move_all);
    }
}

/// Движение на клиенте: предсказание своего персонажа.
pub struct MovementClientPlugin;

impl GamePlugin for MovementClientPlugin {
    fn build(&self, world: &mut World) {
        register_shared(world);
        world.init_resource::<LocalInput>();
        world.init_resource::<Prediction>();
        // Перед применением состояния персонаж возвращается к данным сервера: состояние
        // может его не содержать, если на сервере он не менялся.
        world.subscribe_broadcast::<StateApplying>(|world, _| {
            if let Some((entity, state)) = world.resource::<Prediction>().authoritative {
                restore(world, entity, state);
            }
        });
        world.subscribe_broadcast::<StateApplied>(|world, _| {
            let attached = world.resource::<ClientNet>().attached;
            let authoritative = attached.and_then(|entity| Some((entity, capture(world, entity)?)));
            let mut prediction = world.resource_mut::<Prediction>();
            prediction.authoritative = authoritative;
            prediction.replay = true;
        });
        world.add_game_systems(PreUpdate, predict.after(astrocore_net::client_receive));
    }
}

#[cfg(test)]
mod tests;
