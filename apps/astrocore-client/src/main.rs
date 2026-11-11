//! Клиент Astrocore.
//!
//! Режимы:
//! * `--demo` — локальная демонстрация: здание из вертикального среза, камера-орбита,
//!   несколько направленных спрайтов;
//! * `--connect host:port` — подключение к серверу: реплика мира, предсказание движения,
//!   другие игроки как спрайты, двери по клавише `F`;
//! * `--screenshot файл.png [--frames N]` — сохранить кадр и выйти (проверка рендера без человека);
//! * `--ortho` — начать с ортографической камеры.
//!
//! Управление: WASD — ходьба, Q/E или ←/→ — поворот, ↑/↓ — наклон, `+`/`-` — расстояние,
//! `O` — ортографическая камера, `V` — вид от первого/третьего лица, `F` — дверь рядом.

use std::collections::HashMap;
use std::path::PathBuf;

use astrocore_core::NetworkEntityId;
use astrocore_math::glam::Vec3 as MVec3;
use astrocore_network::client::{ClientCore, ClientEvent};
use astrocore_network::tcp::TcpClient;
use astrocore_render::billboard::{demo_sprite_sheet, spawn_billboard};
use astrocore_render::{
    BillboardPlugin, CameraMode, CameraRig, CameraRigPlugin, DirectionalSprite, RenderedWorld,
    SpriteSheet, WorldMeshPlugin,
};
use astrocore_sprites::DirectionSet;
use astrocore_vertical_slice::Slice;
use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;

const USAGE: &str = "\
astrocore-client — клиент Astrocore

  astrocore-client --demo [--ortho] [--screenshot файл.png [--frames N]]
  astrocore-client --connect 127.0.0.1:7777 [--build имя] [--world-id id]
";

#[derive(Resource, Clone)]
struct Args {
    demo: bool,
    connect: Option<String>,
    build: String,
    world_id: String,
    screenshot: Option<PathBuf>,
    frames: u32,
    ortho: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        demo: false,
        connect: None,
        build: "client".into(),
        world_id: "world".into(),
        screenshot: None,
        frames: 20,
        ortho: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("для {name} нужно значение"));
        match arg.as_str() {
            "--demo" => a.demo = true,
            "--ortho" => a.ortho = true,
            "--connect" => a.connect = Some(value("--connect")?),
            "--build" => a.build = value("--build")?,
            "--world-id" => a.world_id = value("--world-id")?,
            "--screenshot" => a.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--frames" => {
                a.frames = value("--frames")?
                    .parse()
                    .map_err(|e| format!("--frames: {e}"))?
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("неизвестный аргумент {other:?}")),
        }
    }
    if !a.demo && a.connect.is_none() {
        return Err("укажите --demo или --connect".into());
    }
    Ok(a)
}

#[derive(Resource)]
struct Sheet(SpriteSheet);

/// Состояние взгляда игрока.
#[derive(Resource)]
struct Look {
    yaw: f32,
    pitch: f32,
    first_person: bool,
}

struct Net {
    tcp: TcpClient,
    core: ClientCore,
}

#[derive(Resource, Default)]
struct RemoteSprites(HashMap<NetworkEntityId, Entity>);

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("ошибка: {msg}\n");
            }
            eprint!("{USAGE}");
            std::process::exit(if msg.is_empty() { 0 } else { 2 });
        }
    };

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Astrocore".into(),
            resolution: WindowResolution::new(1280, 720),
            ..default()
        }),
        ..default()
    }))
    .add_plugins((WorldMeshPlugin, BillboardPlugin, CameraRigPlugin))
    .insert_resource(Time::<Fixed>::from_hz(20.0))
    .insert_resource(ClearColor(Color::srgb(0.53, 0.66, 0.80)))
    .insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        brightness: 450.0,
        ..default()
    })
    .insert_resource(Look {
        yaw: 0.7,
        pitch: 0.5,
        first_person: false,
    })
    .init_resource::<RemoteSprites>()
    .insert_resource(args.clone())
    .add_systems(Startup, setup)
    .add_systems(Update, (look_input, drive_camera).chain());

    if let Some(addr) = args.connect.clone() {
        let tcp = match TcpClient::connect(&addr) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("не удалось подключиться к {addr}: {e}");
                std::process::exit(1);
            }
        };
        let mut core = ClientCore::new(&args.build, &args.world_id, 0);
        let mut tcp = tcp;
        if let Err(e) = tcp.send(&core.hello_packet()) {
            eprintln!("не удалось отправить Hello: {e}");
            std::process::exit(1);
        }
        let _ = &mut core;
        app.insert_non_send(Net { tcp, core })
            .add_systems(Update, (net_poll, sync_remote_players, door_key).chain())
            .add_systems(FixedUpdate, send_input);
    } else {
        app.add_systems(Startup, setup_demo_world.after(setup));
    }

    if args.screenshot.is_some() {
        app.add_systems(Update, screenshot_and_exit);
    }
    app.run();
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>, args: Res<Args>) {
    let mode = if args.ortho {
        CameraMode::Orthographic
    } else {
        CameraMode::Perspective
    };
    commands.spawn((
        Camera3d::default(),
        CameraRig {
            mode,
            target: Vec3::new(32.0, 4.0, 3.0),
            yaw: 0.7,
            pitch: 0.5,
            distance: 34.0,
            ortho_height: 26.0,
        },
        Transform::default(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.5, 0.0)),
    ));
    commands.insert_resource(Sheet(demo_sprite_sheet(&mut images)));
}

/// Демонстрационный мир: вертикальный срез и четыре спрайта лицом в разные стороны.
fn setup_demo_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    sheet: Res<Sheet>,
) {
    let slice = Slice::build().expect("демо-сцена строится");
    commands.insert_resource(RenderedWorld::new(slice.world));
    // Земля для ориентира и спрайты у южной стены, повёрнутые в разные стороны.
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(240.0, 0.2, 240.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.42, 0.28),
            perceptual_roughness: 1.0,
            ..default()
        })),
        Transform::from_xyz(32.0, -0.1, 3.0),
    ));
    let spots = [
        (Vec3::new(30.0, 0.0, 8.5), Vec3::Z),
        (Vec3::new(31.8, 0.0, 8.5), Vec3::NEG_Z),
        (Vec3::new(33.6, 0.0, 8.5), Vec3::X),
        (Vec3::new(35.4, 0.0, 8.5), Vec3::NEG_X),
    ];
    for (feet, facing) in spots {
        spawn_billboard(
            &mut commands,
            &mut meshes,
            &mut materials,
            &sheet.0,
            feet,
            Vec2::new(0.9, 1.8),
            facing,
            DirectionSet::Four,
        );
    }
}

fn look_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut look: ResMut<Look>,
    time: Res<Time>,
    mut rigs: Query<&mut CameraRig>,
) {
    let dt = time.delta_secs();
    let turn = 1.6 * dt;
    if keys.pressed(KeyCode::KeyQ) || keys.pressed(KeyCode::ArrowLeft) {
        look.yaw += turn;
    }
    if keys.pressed(KeyCode::KeyE) || keys.pressed(KeyCode::ArrowRight) {
        look.yaw -= turn;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        look.pitch = (look.pitch + turn).min(1.4);
    }
    if keys.pressed(KeyCode::ArrowDown) {
        look.pitch = (look.pitch - turn).max(-1.2);
    }
    if keys.just_pressed(KeyCode::KeyV) {
        look.first_person = !look.first_person;
    }
    for mut rig in &mut rigs {
        if keys.pressed(KeyCode::Equal) || keys.pressed(KeyCode::NumpadAdd) {
            rig.distance = (rig.distance - 8.0 * dt).max(2.0);
            rig.ortho_height = (rig.ortho_height - 8.0 * dt).max(4.0);
        }
        if keys.pressed(KeyCode::Minus) || keys.pressed(KeyCode::NumpadSubtract) {
            rig.distance = (rig.distance + 8.0 * dt).min(80.0);
            rig.ortho_height = (rig.ortho_height + 8.0 * dt).min(120.0);
        }
        if keys.just_pressed(KeyCode::KeyO) {
            rig.mode = if rig.mode == CameraMode::Orthographic {
                CameraMode::Perspective
            } else {
                CameraMode::Orthographic
            };
        }
    }
}

/// Камера следует за взглядом: в демо — орбита вокруг здания, в сети — за игроком.
fn drive_camera(look: Res<Look>, net: Option<NonSend<Net>>, mut rigs: Query<&mut CameraRig>) {
    for mut rig in &mut rigs {
        rig.yaw = look.yaw;
        rig.pitch = look.pitch;
        if let Some(net) = net.as_ref() {
            let p = net.core.position();
            rig.target = Vec3::new(p.x, p.y + 1.6, p.z);
            rig.distance = if look.first_person {
                0.0
            } else {
                rig.distance.max(3.0)
            };
        }
    }
}

fn net_poll(
    mut net: NonSendMut<Net>,
    mut rendered: Option<ResMut<RenderedWorld>>,
    mut commands: Commands,
    mut snapshot_serial: Local<u32>,
) {
    let net = &mut *net;
    let mut last_serial = None;
    for packet in net.tcp.poll() {
        let events = match net.core.on_packet(&packet) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("ошибка пакета сервера: {e}");
                continue;
            }
        };
        for ev in events {
            match ev {
                ClientEvent::ResyncRequested => {
                    let _ = net.tcp.send(&net.core.snapshot_request_packet());
                }
                ClientEvent::Rejected { reason, detail } => {
                    eprintln!("сервер отклонил: {reason:?}: {detail}")
                }
                ClientEvent::Joined { you } => println!("вошли в игру: {you:?}"),
                _ => {}
            }
        }
        last_serial = Some(net.core.snapshot_serial());
    }
    let _ = last_serial;
    let Some(world) = net.core.world().cloned() else {
        return;
    };
    let serial = net.core.snapshot_serial();
    match rendered.as_mut() {
        None => commands.insert_resource(RenderedWorld::new(world)),
        Some(r) => {
            let dirty = net.core.take_render_dirty();
            if dirty.is_empty()
                && r.world.revision() == world.revision()
                && *snapshot_serial == serial
            {
                return;
            }
            if *snapshot_serial != serial {
                r.rebuild_all = true; // пришёл новый полный снимок
                *snapshot_serial = serial;
            }
            r.world = world;
            r.dirty.extend(dirty);
        }
    }
}

fn send_input(keys: Res<ButtonInput<KeyCode>>, look: Res<Look>, mut net: NonSendMut<Net>) {
    let axis =
        |neg: KeyCode, pos: KeyCode| i8::from(keys.pressed(pos)) - i8::from(keys.pressed(neg));
    let (move_x, move_z) = (
        axis(KeyCode::KeyA, KeyCode::KeyD),
        axis(KeyCode::KeyS, KeyCode::KeyW),
    );
    let net = &mut *net;
    if let Some(packet) = net.core.input_packet(move_x, move_z, look.yaw) {
        let _ = net.tcp.send(&packet);
    }
}

fn door_key(keys: Res<ButtonInput<KeyCode>>, mut net: NonSendMut<Net>) {
    if !keys.just_pressed(KeyCode::KeyF) {
        return;
    }
    let net = &mut *net;
    let Some(world) = net.core.world() else {
        return;
    };
    let me = net.core.position();
    let step = world.config().grid_step_m;
    let nearest = net
        .core
        .door_ids()
        .into_iter()
        .filter_map(|id| {
            let element = world.element(net.core.door_element(id)?)?;
            let floor = world.floor(element.floor)?;
            let astrocore_world::Placement::Edge(edge) = element.placement else {
                return None;
            };
            let (a, b) = edge.endpoints(step, floor.base_y);
            let center = (a + b) * 0.5;
            Some((id, me.distance(MVec3::new(center.x, me.y, center.z))))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((id, _)) = nearest {
        let _ = net.tcp.send(&net.core.interact_packet(id));
    }
}

/// Другие игроки — направленные спрайты, интерполированные между серверными состояниями.
fn sync_remote_players(
    mut commands: Commands,
    net: NonSend<Net>,
    mut remote: ResMut<RemoteSprites>,
    sheet: Option<Res<Sheet>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sprites: Query<(&mut Transform, &mut DirectionalSprite)>,
) {
    let Some(sheet) = sheet else { return };
    let ids = net.core.remote_entities();
    remote.0.retain(|id, entity| {
        let keep = ids.contains(id);
        if !keep {
            commands.entity(*entity).despawn();
        }
        keep
    });
    let render_tick = net
        .core
        .latest_remote_tick()
        .map_or(0.0, |t| t as f32 - 2.0);
    for id in ids {
        let Some((pos, yaw)) = net.core.interpolated(id, render_tick) else {
            continue;
        };
        let facing = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let feet = Vec3::new(pos.x, pos.y, pos.z);
        match remote.0.get(&id) {
            Some(&entity) => {
                if let Ok((mut t, mut s)) = sprites.get_mut(entity) {
                    t.translation = feet;
                    s.facing = facing;
                }
            }
            None => {
                let e = spawn_billboard(
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &sheet.0,
                    feet,
                    Vec2::new(0.9, 1.8),
                    facing,
                    DirectionSet::Four,
                );
                remote.0.insert(id, e);
            }
        }
    }
}

fn screenshot_and_exit(
    mut commands: Commands,
    args: Res<Args>,
    mut frame: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    if let Some(path) = &args.screenshot {
        if *frame == args.frames {
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(path.clone()));
        }
        if *frame == args.frames + 40 {
            exit.write(AppExit::Success);
        }
    }
}
