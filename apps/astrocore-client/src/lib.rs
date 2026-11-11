use std::collections::HashMap;
use std::path::PathBuf;

use astrocore_core::NetworkEntityId;
use astrocore_math::glam::Vec3 as MVec3;
use astrocore_network::client::{ClientCore, ClientEvent};
use astrocore_network::secure::parse_hex32;
use astrocore_network::tcp::TcpClient;
use astrocore_network::udp::{SystemSocket, UdpClient};
use astrocore_render::billboard::{demo_sprite_sheet, spawn_billboard};
use astrocore_render::{
    BillboardPlugin, CameraMode, CameraRig, CameraRigPlugin, DirectionalSprite, DrawDistancePlugin,
    LightSource, LightingPlugin, ModelLibrary, RenderDiagnosticsPlugin, RenderStats,
    RenderedTerrain, RenderedWorld, ShadowSettings, SpriteSheet, TerrainPlugin, WorldMeshPlugin,
};
use astrocore_sprites::DirectionSet;
use astrocore_vertical_slice::Slice;
use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;

/// Справка по параметрам командной строки.
pub const USAGE: &str = "\
astrocore-client — клиент Astrocore

  astrocore-client ... [--interact-command вид]  клавиша E: игровая команда с ближайшим объектом
  astrocore-client ... [--render-manifest файл.json]  текстуры и спрайты предметов
  astrocore-client --demo [--ortho] [--no-shadows] [--stats] [--screenshot файл.png [--frames N]]
  astrocore-client --connect 127.0.0.1:7777 [--build имя] [--world-id id] [--assets папка]
  astrocore-client --connect-lightyear 127.0.0.1:7779 --ly-key <hex32> [--assets папка]
  astrocore-client --connect-udp 127.0.0.1:7778 --server-key <hex32> [--assets папка]
";

/// Параметры клиента (разобранная командная строка).
#[derive(Resource, Clone)]
pub struct Args {
    pub demo: bool,
    pub connect: Option<String>,
    pub connect_udp: Option<String>,
    pub connect_lightyear: Option<String>,
    pub ly_key: Option<[u8; 32]>,
    pub server_key: Option<[u8; 32]>,
    pub assets: Option<PathBuf>,
    pub render_manifest: Option<PathBuf>,
    pub camera: Option<(f32, f32, f32)>,
    pub debug_doors_open: bool,
    pub interact_command: Option<String>,
    pub build: String,
    pub world_id: String,
    pub screenshot: Option<PathBuf>,
    pub frames: u32,
    pub ortho: bool,
    pub shadows: bool,
    pub stats: bool,
}

/// Разобрать аргументы; `Err` с пустым текстом — запрошена справка.
pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        demo: false,
        connect: None,
        connect_udp: None,
        connect_lightyear: None,
        ly_key: None,
        server_key: None,
        assets: None,
        render_manifest: None,
        camera: None,
        debug_doors_open: false,
        interact_command: None,
        build: "client".into(),
        world_id: "world".into(),
        screenshot: None,
        frames: 20,
        ortho: false,
        shadows: true,
        stats: false,
    };
    let mut it = args;
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("для {name} нужно значение"));
        match arg.as_str() {
            "--demo" => a.demo = true,
            "--ortho" => a.ortho = true,
            "--no-shadows" => a.shadows = false,
            "--stats" => a.stats = true,
            "--connect" => a.connect = Some(value("--connect")?),
            "--connect-udp" => a.connect_udp = Some(value("--connect-udp")?),
            "--server-key" => {
                a.server_key = Some(
                    parse_hex32(&value("--server-key")?)
                        .ok_or("--server-key: нужно 64 шестнадцатеричных символа")?,
                )
            }
            "--connect-lightyear" => a.connect_lightyear = Some(value("--connect-lightyear")?),
            "--ly-key" => {
                a.ly_key = Some(
                    parse_hex32(&value("--ly-key")?)
                        .ok_or("--ly-key: нужно 64 шестнадцатеричных символа")?,
                )
            }
            "--render-manifest" => {
                a.render_manifest = Some(PathBuf::from(value("--render-manifest")?))
            }
            "--camera" => {
                let v: Vec<f32> = value("--camera")?
                    .split(',')
                    .map(|p| p.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("--camera: {e}"))?;
                let [yaw, pitch, distance] = v[..] else {
                    return Err("--camera: ожидается yaw,pitch,distance".into());
                };
                a.camera = Some((yaw, pitch, distance));
            }
            "--interact-command" => a.interact_command = Some(value("--interact-command")?),
            "--debug-doors-open" => a.debug_doors_open = true,
            "--assets" => a.assets = Some(PathBuf::from(value("--assets")?)),
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
    if !a.demo && a.connect.is_none() && a.connect_udp.is_none() && a.connect_lightyear.is_none() {
        return Err("укажите --demo, --connect, --connect-udp или --connect-lightyear".into());
    }
    if a.connect_lightyear.is_some() && a.ly_key.is_none() {
        return Err("для --connect-lightyear нужен --ly-key".into());
    }
    if a.connect_udp.is_some() && a.server_key.is_none() {
        return Err("для --connect-udp нужен --server-key".into());
    }
    Ok(a)
}

#[derive(Resource)]
struct Sheet(SpriteSheet);

/// Спрайт игрока от игры: лист направлений и размер в метрах. Если ресурс есть, клиент рисует им
/// и чужих игроков, и своё тело (в виде от третьего лица); иначе — демонстрационный человечек.
#[derive(Resource, Clone)]
pub struct PlayerSprite {
    pub sheet: SpriteSheet,
    pub size: Vec2,
}

/// Состояние взгляда игрока.
#[derive(Resource)]
struct Look {
    yaw: f32,
    pitch: f32,
    first_person: bool,
}

enum Transport {
    Tcp(TcpClient),
    Udp(Box<UdpClient<SystemSocket>>),
    Lightyear(Box<astrocore_lightyear::LyClient>),
}

impl Transport {
    fn send(&mut self, reliable: bool, data: &[u8]) {
        match self {
            Transport::Tcp(t) => {
                let _ = t.send(data);
            }
            Transport::Udp(u) => {
                let _ = u.send(reliable, data);
            }
            Transport::Lightyear(l) => {
                l.send(reliable, data.to_vec());
            }
        }
    }

    fn poll(&mut self, now_ms: u64) -> Vec<Vec<u8>> {
        match self {
            Transport::Tcp(t) => t.poll(),
            Transport::Lightyear(l) => {
                l.step();
                l.take_packets()
            }
            Transport::Udp(u) => {
                if let Err(e) = u.step(now_ms) {
                    eprintln!("ошибка сокета: {e}");
                }
                if let Some(e) = u.error() {
                    eprintln!("соединение потеряно: {e}");
                }
                u.take_packets()
            }
        }
    }
}

/// Подключение к серверу: транспорт и клиентское ядро (реплика мира, предсказание). Игровые
/// расширения клиента получают его как `NonSend` ресурс и шлют команды через `send_command`.
pub struct Net {
    transport: Transport,
    pub core: ClientCore,
    started: std::time::Instant,
}

impl Net {
    /// Отправить игровую команду серверу (надёжным каналом).
    pub fn send_command(&mut self, kind: &str, data: Vec<u8>) -> bool {
        match self.core.command_packet(kind, data) {
            Some(packet) => {
                self.transport.send(true, &packet);
                true
            }
            None => false,
        }
    }
}

#[derive(Resource, Default)]
struct RemoteSprites(HashMap<NetworkEntityId, Entity>);

/// Собрать приложение клиента. Игра добавляет свои плагины и запускает `app.run()`.
pub fn build_app(args: Args) -> App {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Astrocore".into(),
            resolution: WindowResolution::new(1280, 720),
            ..default()
        }),
        ..default()
    }))
    .add_plugins((
        WorldMeshPlugin,
        TerrainPlugin,
        BillboardPlugin,
        CameraRigPlugin,
        LightingPlugin,
        RenderDiagnosticsPlugin,
        DrawDistancePlugin,
        astrocore_render::PlacementSpritesPlugin,
        astrocore_render::DoorSpritesPlugin,
    ))
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
    .insert_resource(ShadowSettings {
        sun: args.shadows,
        local_lights: args.shadows,
        ..default()
    })
    .init_resource::<RemoteSprites>()
    .insert_resource(args.clone())
    .add_systems(Startup, (setup, load_render_manifest))
    .add_systems(Update, (look_input, drive_camera).chain());

    if args.connect.is_some() || args.connect_udp.is_some() || args.connect_lightyear.is_some() {
        let mut transport = if let Some(addr) = args.connect_lightyear.clone() {
            let server: std::net::SocketAddr = match addr.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("--connect-lightyear: {e}");
                    std::process::exit(1);
                }
            };
            let id = std::process::id() as u64 ^ 0x4153_0000;
            match astrocore_lightyear::LyClient::connect(
                server,
                id,
                args.ly_key.unwrap_or_default(),
                60,
            ) {
                Ok(c) => Transport::Lightyear(Box::new(c)),
                Err(e) => {
                    eprintln!("не удалось начать соединение Lightyear: {e}");
                    std::process::exit(1);
                }
            }
        } else if let Some(addr) = args.connect.clone() {
            match TcpClient::connect(&addr) {
                Ok(t) => Transport::Tcp(t),
                Err(e) => {
                    eprintln!("не удалось подключиться к {addr}: {e}");
                    std::process::exit(1);
                }
            }
        } else {
            let addr = args.connect_udp.clone().unwrap_or_default();
            let server: std::net::SocketAddr = match addr.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("--connect-udp: {e}");
                    std::process::exit(1);
                }
            };
            let socket = match SystemSocket::bind("0.0.0.0:0") {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("не удалось открыть сокет: {e}");
                    std::process::exit(1);
                }
            };
            match UdpClient::connect(socket, server, args.server_key.unwrap_or_default()) {
                Ok(u) => Transport::Udp(Box::new(u)),
                Err(e) => {
                    eprintln!("не удалось начать соединение: {e}");
                    std::process::exit(1);
                }
            }
        };
        let core = ClientCore::new(&args.build, &args.world_id, 0);
        transport.send(true, &core.hello_packet());
        if let Some(dir) = args.assets.clone() {
            app.insert_resource(ModelLibrary::new(dir));
        }
        app.insert_non_send(Net {
            transport,
            core,
            started: std::time::Instant::now(),
        })
        .add_systems(
            Update,
            (
                net_poll,
                sync_door_states,
                sync_remote_players,
                sync_local_body,
                door_key,
                interact_key,
            )
                .chain(),
        )
        .add_systems(FixedUpdate, send_input);
    } else {
        app.add_systems(Startup, setup_demo_world.after(setup));
    }

    if args.stats {
        app.add_systems(Update, print_stats);
    }
    if args.screenshot.is_some() {
        app.add_systems(Update, screenshot_and_exit);
    }
    app
}

/// Текстуры материалов и спрайты предметов из манифеста рендера (`--render-manifest`).
fn load_render_manifest(
    args: Res<Args>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
) {
    let Some(path) = &args.render_manifest else {
        return;
    };
    let base = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let result =
        astrocore_render::RenderManifest::load(path).and_then(|m| m.install(&base, &mut images));
    match result {
        Ok((textures, sprites, doors)) => {
            println!(
                "манифест рендера: текстур {}, спрайтов {}, дверей {}",
                textures.0.len(),
                sprites.0.len(),
                doors.0.len()
            );
            commands.insert_resource(textures);
            commands.insert_resource(sprites);
            commands.insert_resource(doors);
        }
        Err(e) => eprintln!("манифест рендера не загружен: {e}"),
    }
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>, args: Res<Args>) {
    let mode = if args.ortho {
        CameraMode::Orthographic
    } else {
        CameraMode::Perspective
    };
    let (cam_yaw, cam_pitch, cam_distance) = args.camera.unwrap_or((0.7, 0.5, 34.0));
    commands.insert_resource(Look {
        yaw: cam_yaw,
        pitch: cam_pitch,
        first_person: false,
    });
    commands.spawn((
        Camera3d::default(),
        CameraRig {
            mode,
            target: Vec3::new(32.0, 4.0, 3.0),
            yaw: cam_yaw,
            pitch: cam_pitch,
            distance: cam_distance,
            ortho_height: 26.0,
        },
        Transform::default(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadow_maps_enabled: true,
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
    commands.insert_resource(RenderedTerrain::new(demo_terrain()));
    commands.spawn((
        LightSource::point(Color::srgb(1.0, 0.85, 0.6), 400_000.0, 14.0),
        Transform::from_xyz(32.0, 3.0, 3.0),
    ));
    // Спрайты у южной стены, повёрнутые в разные стороны.
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
    args: Res<Args>,
    mut look: ResMut<Look>,
    time: Res<Time>,
    mut rigs: Query<&mut CameraRig>,
) {
    let dt = time.delta_secs();
    let turn = 1.6 * dt;
    if keys.pressed(KeyCode::KeyQ) || keys.pressed(KeyCode::ArrowLeft) {
        look.yaw += turn;
    }
    // Если E отдана взаимодействию (`--interact-command`), поворот вправо — только стрелкой.
    let turn_right_key = args.interact_command.is_none() && keys.pressed(KeyCode::KeyE);
    if turn_right_key || keys.pressed(KeyCode::ArrowRight) {
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
    mut rendered_terrain: Option<ResMut<RenderedTerrain>>,
    mut commands: Commands,
    mut snapshot_serial: Local<u32>,
    mut terrain_serial: Local<u32>,
) {
    let net = &mut *net;
    let now_ms = net.started.elapsed().as_millis() as u64;
    for packet in net.transport.poll(now_ms) {
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
                    net.transport
                        .send(true, &net.core.snapshot_request_packet());
                }
                ClientEvent::Rejected { reason, detail } => {
                    eprintln!("сервер отклонил: {reason:?}: {detail}")
                }
                ClientEvent::Joined { you } => println!("вошли в игру: {you:?}"),
                _ => {}
            }
        }
    }
    sync_terrain(
        net,
        &mut rendered_terrain,
        &mut commands,
        &mut terrain_serial,
    );
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

fn sync_terrain(
    net: &mut Net,
    rendered: &mut Option<ResMut<RenderedTerrain>>,
    commands: &mut Commands,
    serial: &mut u32,
) {
    if net.core.terrain().is_none() {
        return;
    }
    let new_serial = net.core.terrain_serial();
    let dirty = net.core.take_terrain_dirty();
    if dirty.is_empty() && *serial == new_serial && rendered.is_some() {
        return;
    }
    let Some(terrain) = net.core.terrain().cloned() else {
        return;
    };
    match rendered.as_mut() {
        None => {
            commands.insert_resource(RenderedTerrain::new(terrain));
            *serial = new_serial;
        }
        Some(r) => {
            if *serial != new_serial {
                r.rebuild_all = true;
                *serial = new_serial;
            }
            r.terrain = terrain;
            r.dirty.extend(dirty);
        }
    }
}

/// Передать рендеру серверное состояние дверей (открыта или закрыта).
fn sync_door_states(
    net: NonSend<Net>,
    args: Res<Args>,
    mut states: ResMut<astrocore_render::DoorStates>,
) {
    // Отладка внешнего вида: только на экране, серверное состояние не меняется.
    let force_open = args.debug_doors_open;
    for id in net.core.door_ids() {
        if let (Some(element), Some(open)) = (net.core.door_element(id), net.core.door_open(id))
            && states.0.get(&element) != Some(&(open || force_open))
        {
            states.0.insert(element, open || force_open);
        }
    }
}

/// E: игровая команда взаимодействия с ближайшей целью. Вид команды задаёт игра
/// (`--interact-command`), данные — идентификатор объекта мира (16 байт, big-endian): предмет
/// на земле (авторский объект) или дверь (структурный элемент).
fn interact_key(keys: Res<ButtonInput<KeyCode>>, args: Res<Args>, mut net: NonSendMut<Net>) {
    let Some(kind) = args.interact_command.as_deref() else {
        return;
    };
    if !keys.just_pressed(KeyCode::KeyE) {
        return;
    }
    let net = &mut *net;
    let Some(world) = net.core.world() else {
        return;
    };
    let me = net.core.position();
    let chest = MVec3::new(me.x, me.y + 1.0, me.z);
    let mut best: Option<(f32, astrocore_core::ObjectId)> = None;
    let mut consider = |id: astrocore_core::ObjectId, at: MVec3| {
        let d = at.distance(chest);
        if d <= 3.0 && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, id));
        }
    };
    for o in world.authored() {
        if matches!(o.kind, astrocore_world::AuthoredKind::Placement { .. }) {
            consider(o.id, (o.min() + o.max()) * 0.5);
        }
    }
    let step = world.config().grid_step_m;
    for door in net.core.door_ids() {
        let Some(id) = net.core.door_element(door) else {
            continue;
        };
        let Some(e) = world.element(id) else { continue };
        let (Some(floor), astrocore_world::Placement::Edge(edge)) =
            (world.floor(e.floor), e.placement)
        else {
            continue;
        };
        let (a, b) = edge.endpoints(step, floor.base_y);
        consider(
            id,
            MVec3::new((a.x + b.x) * 0.5, floor.base_y + 1.0, (a.z + b.z) * 0.5),
        );
    }
    if let Some((_, id)) = best
        && let Some(packet) = net
            .core
            .command_packet(kind, id.as_uuid().as_u128().to_be_bytes().to_vec())
    {
        net.transport.send(true, &packet);
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
        net.transport.send(false, &packet);
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
        net.transport.send(true, &net.core.interact_packet(id));
    }
}

/// Другие игроки — направленные спрайты, интерполированные между серверными состояниями.
fn sync_remote_players(
    mut commands: Commands,
    net: NonSend<Net>,
    mut remote: ResMut<RemoteSprites>,
    sheet: Option<Res<Sheet>>,
    player: Option<Res<PlayerSprite>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sprites: Query<(&mut Transform, &mut DirectionalSprite)>,
) {
    let (sheet, size) = match (&player, &sheet) {
        (Some(p), _) => (p.sheet.clone(), p.size),
        (None, Some(s)) => (s.0.clone(), Vec2::new(0.9, 1.8)),
        _ => return,
    };
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
                    &sheet,
                    feet,
                    size,
                    facing,
                    DirectionSet::Four,
                );
                remote.0.insert(id, e);
            }
        }
    }
}

/// Своё тело: спрайт игрока в точке, где его держит сервер; в виде от первого лица скрыт.
fn sync_local_body(
    mut commands: Commands,
    net: NonSend<Net>,
    player: Option<Res<PlayerSprite>>,
    look: Res<Look>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut body: Local<Option<Entity>>,
    mut sprites: Query<(&mut Transform, &mut DirectionalSprite, &mut Visibility)>,
) {
    let Some(player) = player else { return };
    if net.core.net_id().is_none() {
        return;
    }
    let p = net.core.position();
    let feet = Vec3::new(p.x, p.y, p.z);
    let facing = Vec3::new(-look.yaw.sin(), 0.0, -look.yaw.cos());
    match *body {
        Some(e) => {
            if let Ok((mut t, mut s, mut v)) = sprites.get_mut(e) {
                t.translation = feet;
                s.facing = facing;
                *v = if look.first_person {
                    Visibility::Hidden
                } else {
                    Visibility::Inherited
                };
            }
        }
        None => {
            *body = Some(spawn_billboard(
                &mut commands,
                &mut meshes,
                &mut materials,
                &player.sheet,
                feet,
                player.size,
                facing,
                DirectionSet::Four,
            ));
        }
    }
}

fn print_stats(stats: Res<RenderStats>, time: Res<Time>, mut last: Local<f32>) {
    if time.elapsed_secs() - *last >= 2.0 {
        *last = time.elapsed_secs();
        println!("рендер: {}", stats.summary());
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

/// Демонстрационный рельеф: плоская площадка под зданием и холмы вокруг.
fn demo_terrain() -> astrocore_terrain::Terrain {
    use astrocore_math::glam::Vec2;
    let mut t = astrocore_terrain::Terrain::new(1.0, 16, 0.0).expect("конфиг рельефа");
    // Пологая подложка: рельеф хранит только изменённые чанки, остальное — плоская база.
    for (c, r, h) in [
        (Vec2::new(32.0, 24.0), 64.0, 0.15),
        (Vec2::new(8.0, 30.0), 9.0, 3.0),
        (Vec2::new(58.0, 26.0), 11.0, 4.0),
        (Vec2::new(32.0, 40.0), 8.0, 2.0),
    ] {
        let e = t.brush_raise(c, r, h);
        let _ = t.apply(&e);
    }
    let _ = t.take_dirty();
    t
}
