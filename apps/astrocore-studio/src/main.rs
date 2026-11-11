//! Astrocore Studio: окно редактора карт. Логика — в `astrocore_studio::StudioState`
//! (тестируется без окна); здесь только Bevy + egui, камера, указатель и отрисовка подсказок.
//!
//! Управление: ЛКМ — инструмент; ПКМ/СКМ + движение — вращение камеры; колесо — масштаб;
//! WASD — сдвиг камеры; Ctrl+Z / Ctrl+Y — отмена/повтор; Esc — отменить текущее действие.
//! `--screenshot файл.png [--frames N]` — сохранить кадр и выйти (проверка без человека).

use std::path::PathBuf;

use astrocore_editor::view::FloorView;
use astrocore_render::{
    BillboardPlugin, CameraMode, CameraRig, CameraRigPlugin, RenderedWorld, WorldMeshPlugin,
};
use astrocore_studio::{Pointer, Preview, StudioState, Tool};
use bevy::app::AppExit;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{PrimaryWindow, WindowResolution};
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};

#[derive(Resource)]
struct Args {
    screenshot: Option<PathBuf>,
    frames: u32,
    demo_room: bool,
}

#[derive(Resource)]
struct Studio(StudioState);

/// Что рендер уже знает об изменениях.
#[derive(Resource, Default)]
struct SyncState {
    stamp: u64,
    revision: u64,
    view: Option<FloorView>,
    path_text: String,
    playtest: Option<String>,
}

fn main() {
    let mut args = Args {
        screenshot: None,
        frames: 30,
        demo_room: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--frames" => args.frames = it.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--demo-room" => args.demo_room = true,
            "-h" | "--help" => {
                eprintln!("astrocore-studio [--screenshot файл.png [--frames N]] [--demo-room]");
                return;
            }
            other => {
                eprintln!("неизвестный аргумент {other:?}");
                std::process::exit(2);
            }
        }
    }

    let mut state = StudioState::new();
    if args.demo_room {
        build_demo(&mut state);
    }
    let world = state.world().clone();

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Astrocore Studio".into(),
                resolution: WindowResolution::new(1440, 800),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .add_plugins((WorldMeshPlugin, BillboardPlugin, CameraRigPlugin))
        .insert_resource(ClearColor(Color::srgb(0.18, 0.2, 0.24)))
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 500.0,
            ..default()
        })
        .insert_resource(RenderedWorld::new(world))
        .insert_resource(Studio(state))
        .insert_resource(args)
        .init_resource::<SyncState>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                camera_controls,
                pointer_tools,
                hotkeys,
                sync_render,
                draw_helpers,
                screenshot_and_exit,
            )
                .chain(),
        )
        .add_systems(EguiPrimaryContextPass, ui)
        .run();
}

/// Демонстрационная карта для скриншота: две комнаты, дверь, лестница на второй этаж.
fn build_demo(s: &mut StudioState) {
    let p = |x: f32, z: f32| Vec3::new(x, 0.0, z);
    let drag = |s: &mut StudioState, a: Vec3, b: Vec3| {
        s.pointer(Pointer::Press(a));
        s.pointer(Pointer::Release(b));
    };
    s.material = "studio:wall".into();
    s.add_floor();
    s.floor = astrocore_world::FloorId(0);
    s.room_ceiling = false;
    s.tool = Tool::Room;
    drag(s, p(0.5, 0.5), p(6.5, 5.5));
    drag(s, p(7.5, 0.5), p(12.5, 5.5));
    s.tool = Tool::Door;
    drag(s, p(7.0, 2.5), p(7.0, 2.5));
    s.floor = astrocore_world::FloorId(1);
    s.tool = Tool::Room;
    drag(s, p(0.5, 0.5), p(6.5, 5.5));
    s.floor = astrocore_world::FloorId(0);
    s.tool = Tool::Stair;
    s.stair_len = 4;
    drag(s, p(1.5, 3.5), p(1.5, 3.5));
    s.validate();
    s.tool = Tool::Select;
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        CameraRig {
            mode: CameraMode::Perspective,
            target: Vec3::new(6.0, 0.0, 3.0),
            yaw: 0.5,
            pitch: 0.9,
            distance: 22.0,
            ortho_height: 18.0,
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
}

fn egui_wants_pointer(contexts: &mut EguiContexts) -> bool {
    contexts
        .ctx_mut()
        .map(|c| c.egui_wants_pointer_input() || c.is_pointer_over_egui())
        .unwrap_or(false)
}

fn egui_wants_keyboard(contexts: &mut EguiContexts) -> bool {
    contexts
        .ctx_mut()
        .map(|c| c.egui_wants_keyboard_input())
        .unwrap_or(false)
}

fn camera_controls(
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    time: Res<Time>,
    mut rigs: Query<&mut CameraRig>,
) {
    let over_ui = egui_wants_pointer(&mut contexts);
    let kb_ui = egui_wants_keyboard(&mut contexts);
    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    let scroll: f32 = wheel.read().map(|w| w.y).sum();
    for mut rig in &mut rigs {
        if !over_ui {
            if mouse.pressed(MouseButton::Right) || mouse.pressed(MouseButton::Middle) {
                rig.yaw -= delta.x * 0.005;
                rig.pitch = (rig.pitch + delta.y * 0.005).clamp(0.05, 1.5);
            }
            rig.distance = (rig.distance * (1.0 - scroll * 0.1)).clamp(3.0, 200.0);
            rig.ortho_height = (rig.ortho_height * (1.0 - scroll * 0.1)).clamp(4.0, 300.0);
        }
        if !kb_ui {
            let speed = rig.distance.max(6.0) * 0.6 * time.delta_secs();
            let (s, c) = rig.yaw.sin_cos();
            let (fwd, right) = (Vec3::new(-s, 0.0, -c), Vec3::new(c, 0.0, -s));
            for (key, dir) in [
                (KeyCode::KeyW, fwd),
                (KeyCode::KeyS, -fwd),
                (KeyCode::KeyD, right),
                (KeyCode::KeyA, -right),
            ] {
                if keys.pressed(key) {
                    rig.target += dir * speed;
                }
            }
        }
    }
}

/// Точка под курсором на плоскости текущего этажа.
fn cursor_on_plane(
    window: &Window,
    camera: &Camera,
    transform: &GlobalTransform,
    plane_y: f32,
) -> Option<Vec3> {
    let cursor = window.cursor_position()?;
    let ray = camera.viewport_to_world(transform, cursor).ok()?;
    let dir: Vec3 = ray.direction.into();
    if dir.y.abs() < 1e-6 {
        return None;
    }
    let t = (plane_y - ray.origin.y) / dir.y;
    (t > 0.0).then(|| ray.origin + dir * t)
}

fn pointer_tools(
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    mut studio: ResMut<Studio>,
) {
    let over_ui = egui_wants_pointer(&mut contexts);
    let (Ok(window), Ok((camera, transform))) = (windows.single(), cameras.single()) else {
        return;
    };
    studio.0.additive_select =
        keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let plane_y = studio.0.floor_base_y();
    let Some(point) = cursor_on_plane(window, camera, transform, plane_y) else {
        return;
    };
    if mouse.just_pressed(MouseButton::Left) && !over_ui {
        studio.0.pointer(Pointer::Press(point));
    } else if mouse.pressed(MouseButton::Left) {
        studio.0.pointer(Pointer::Drag(point));
    }
    if mouse.just_released(MouseButton::Left) {
        studio.0.pointer(Pointer::Release(point));
    }
}

fn hotkeys(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut studio: ResMut<Studio>,
) {
    if egui_wants_keyboard(&mut contexts) {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if ctrl && keys.just_pressed(KeyCode::KeyZ) {
        studio.0.undo();
    }
    if ctrl && keys.just_pressed(KeyCode::KeyY) {
        studio.0.redo();
    }
    if keys.just_pressed(KeyCode::Escape) {
        studio.0.cancel();
    }
    if keys.just_pressed(KeyCode::Delete) {
        studio.0.tool = Tool::Delete;
    }
}

/// Передать рендеру изменения: dirty-регионы сессии или полную пересборку.
fn sync_render(
    mut studio: ResMut<Studio>,
    mut rendered: ResMut<RenderedWorld>,
    mut sync: ResMut<SyncState>,
) {
    let s = &mut studio.0;
    if sync.stamp == s.stamp && sync.view == Some(s.view) {
        return;
    }
    let revision = s.world().revision().0;
    let view_changed = sync.view != Some(s.view);
    let regressed = revision < sync.revision; // открыт другой проект
    rendered.world = s.world().clone();
    if view_changed || regressed {
        rendered.filter = s.render_filter();
        rendered.rebuild_all = true;
        s.session.take_dirty();
    } else {
        let dirty = s.session.take_dirty();
        rendered.dirty.extend(dirty);
    }
    sync.stamp = s.stamp;
    sync.revision = revision;
    sync.view = Some(s.view);
}

/// Сетка текущего этажа, подсказка действия и рамки выделения.
fn draw_helpers(mut gizmos: Gizmos, studio: Res<Studio>, rigs: Query<&CameraRig>) {
    let s = &studio.0;
    let y = s.floor_base_y() + 0.02;
    let center = rigs.single().map_or(Vec3::ZERO, |r| r.target);
    let (cx, cz) = (center.x.round() as i32, center.z.round() as i32);
    let grid = Color::srgba(1.0, 1.0, 1.0, 0.12);
    for i in -24..=24 {
        gizmos.line(
            Vec3::new((cx + i) as f32, y, (cz - 24) as f32),
            Vec3::new((cx + i) as f32, y, (cz + 24) as f32),
            grid,
        );
        gizmos.line(
            Vec3::new((cx - 24) as f32, y, (cz + i) as f32),
            Vec3::new((cx + 24) as f32, y, (cz + i) as f32),
            grid,
        );
    }
    gizmos.line(
        Vec3::new(-100.0, y, 0.0),
        Vec3::new(100.0, y, 0.0),
        Color::srgb(0.9, 0.3, 0.3),
    );
    gizmos.line(
        Vec3::new(0.0, y, -100.0),
        Vec3::new(0.0, y, 100.0),
        Color::srgb(0.3, 0.4, 0.95),
    );
    let ghost = Color::srgb(1.0, 0.85, 0.2);
    match s.preview {
        Preview::Line(a, b) => gizmos.line(a + Vec3::Y * 0.05, b + Vec3::Y * 0.05, ghost),
        Preview::Rect(a, b) => {
            let (a, b) = (a + Vec3::Y * 0.05, b + Vec3::Y * 0.05);
            let (c, d) = (Vec3::new(a.x, a.y, b.z), Vec3::new(b.x, a.y, a.z));
            for (p, q) in [(a, c), (c, b), (b, d), (d, a)] {
                gizmos.line(p, q, ghost);
            }
        }
        Preview::None => {}
    }
    let sel = Color::srgb(0.2, 1.0, 0.4);
    for id in &s.selection {
        let Some(e) = s.world().element(*id) else {
            continue;
        };
        let Some(f) = s.world().floor(e.floor) else {
            continue;
        };
        let Some(b) = astrocore_geometry_bounds(s.world(), f, e) else {
            continue;
        };
        let (lo, hi) = (
            Vec3::new(b.0[0], b.0[1], b.0[2]),
            Vec3::new(b.1[0], b.1[1], b.1[2]),
        );
        let corners = |yv: f32| {
            [
                Vec3::new(lo.x, yv, lo.z),
                Vec3::new(hi.x, yv, lo.z),
                Vec3::new(hi.x, yv, hi.z),
                Vec3::new(lo.x, yv, hi.z),
            ]
        };
        let (bot, top) = (corners(lo.y), corners(hi.y));
        for i in 0..4 {
            gizmos.line(bot[i], bot[(i + 1) % 4], sel);
            gizmos.line(top[i], top[(i + 1) % 4], sel);
            gizmos.line(bot[i], top[i], sel);
        }
    }
}

/// Габариты элемента для рамки выделения (в метрах): (min, max).
fn astrocore_geometry_bounds(
    world: &astrocore_world::World,
    floor: &astrocore_world::FloorDef,
    e: &astrocore_world::StructureElement,
) -> Option<([f32; 3], [f32; 3])> {
    let boxes = if matches!(e.kind, astrocore_world::StructureKind::Opening(_)) {
        astrocore_geometry::opening_bounds(world, e)
            .into_iter()
            .collect()
    } else {
        astrocore_geometry::collision_boxes(world, floor, e)
    };
    let mut it = boxes.into_iter();
    let first = it.next()?;
    let all = it.fold(first, |a, b| a.union(&b));
    Some((all.min.to_array(), all.max.to_array()))
}

fn ui(
    mut contexts: EguiContexts,
    mut studio: ResMut<Studio>,
    mut sync: ResMut<SyncState>,
    mut rigs: Query<&mut CameraRig>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let mut root = egui::Ui::new(
        ctx.clone(),
        "studio_root".into(),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    let s = &mut studio.0;

    egui::Panel::top("menu").show(&mut root, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Файл", |ui| {
                if ui.button("Новый").clicked() {
                    s.new_project();
                    ui.close();
                }
                ui.horizontal(|ui| {
                    ui.label("Путь:");
                    ui.text_edit_singleline(&mut sync.path_text);
                });
                if ui.button("Открыть").clicked() && !sync.path_text.is_empty() {
                    s.load(std::path::Path::new(&sync.path_text));
                    ui.close();
                }
                if ui.button("Сохранить").clicked() && !sync.path_text.is_empty() {
                    s.save(std::path::Path::new(&sync.path_text));
                    ui.close();
                }
            });
            ui.menu_button("Правка", |ui| {
                if ui
                    .add_enabled(s.session.can_undo(), egui::Button::new("Отменить (Ctrl+Z)"))
                    .clicked()
                {
                    s.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        s.session.can_redo(),
                        egui::Button::new("Повторить (Ctrl+Y)"),
                    )
                    .clicked()
                {
                    s.redo();
                    ui.close();
                }
            });
            ui.separator();
            ui.label("Вид:");
            let mut view = s.view;
            egui::ComboBox::from_id_salt("view")
                .selected_text(match view {
                    FloorView::All => "Всё здание",
                    FloorView::Slice { .. } => "Срез этажа",
                    FloorView::XRay { .. } => "Рентген",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut view, FloorView::All, "Всё здание");
                    ui.selectable_value(&mut view, FloorView::Slice { top: s.floor }, "Срез этажа");
                    ui.selectable_value(&mut view, FloorView::XRay { focus: s.floor }, "Рентген");
                });
            if view != s.view {
                s.set_view(view);
            }
            if let Ok(mut rig) = rigs.single_mut()
                && ui
                    .button(if rig.mode == CameraMode::Orthographic {
                        "Перспектива"
                    } else {
                        "Ортография"
                    })
                    .clicked()
            {
                rig.mode = if rig.mode == CameraMode::Orthographic {
                    CameraMode::Perspective
                } else {
                    CameraMode::Orthographic
                };
            }
            ui.separator();
            if ui.button("▶ Играть отсюда").clicked() {
                sync.playtest = Some(start_playtest(s));
            }
        });
    });

    egui::Panel::left("tools")
        .default_size(240.0)
        .show(&mut root, |ui| {
            ui.heading("Инструменты");
            for tool in Tool::ALL {
                ui.selectable_value(&mut s.tool, tool, tool.title());
            }
            ui.separator();
            ui.label("Материал (ID)");
            ui.text_edit_singleline(&mut s.material);
            ui.add(egui::Slider::new(&mut s.thickness, 0.05..=1.0).text("толщина"));
            match s.tool {
                Tool::Room => {
                    ui.checkbox(&mut s.room_floor, "пол");
                    ui.checkbox(&mut s.room_ceiling, "потолок");
                }
                Tool::Stair => {
                    ui.checkbox(&mut s.stair_ramp, "пандус");
                    ui.add(egui::Slider::new(&mut s.stair_len, 1..=16).text("длина"));
                    egui::ComboBox::from_label("направление")
                        .selected_text(format!("{:?}", s.stair_dir))
                        .show_ui(ui, |ui| {
                            for d in [
                                astrocore_math::Facing::East,
                                astrocore_math::Facing::West,
                                astrocore_math::Facing::South,
                                astrocore_math::Facing::North,
                            ] {
                                ui.selectable_value(&mut s.stair_dir, d, format!("{d:?}"));
                            }
                        });
                }
                _ => {}
            }
            ui.separator();
            ui.heading("Этажи");
            let floors: Vec<_> = s
                .world()
                .floors()
                .map(|f| (f.id, f.name.clone(), f.base_y))
                .collect();
            for (id, name, base) in floors {
                ui.selectable_value(&mut s.floor, id, format!("{name} (y = {base:.1} м)"));
            }
            if ui.button("＋ Этаж сверху").clicked() {
                s.add_floor();
            }
            ui.separator();
            ui.heading("Автоматика");
            if ui.button("Пол по контуру").clicked() {
                s.auto_slabs(false);
            }
            if ui.button("Потолок по контуру").clicked() {
                s.auto_slabs(true);
            }
            if ui.button("Валидатор").clicked() {
                s.validate();
            }
        });

    egui::Panel::right("inspector")
        .default_size(280.0)
        .show(&mut root, |ui| {
            ui.heading("Свойства");
            if s.selection.is_empty() {
                ui.label("Ничего не выбрано");
            }
            let items: Vec<_> = s
                .selection
                .iter()
                .filter_map(|id| {
                    s.world()
                        .element(*id)
                        .map(|e| (e.id, e.kind, e.placement, e.material.clone(), e.floor))
                })
                .collect();
            for (id, kind, placement, material, floor) in items.iter().take(12) {
                ui.group(|ui| {
                    ui.label(format!("{kind:?}"));
                    ui.label(format!("этаж {}, {material}", floor.0));
                    ui.small(format!("{placement:?}"));
                    ui.small(format!("{id}"));
                });
            }
            if items.len() > 12 {
                ui.label(format!("… и ещё {}", items.len() - 12));
            }
            if !s.selection.is_empty() && ui.button("Заменить материал на указанный").clicked()
            {
                s.replace_selected_material();
            }
            ui.separator();
            ui.heading("Проверка карты");
            for f in s.findings.iter().take(30) {
                ui.label(format!("{:?}: {}", f.severity, f.message));
            }
            if s.findings.is_empty() {
                ui.small("Нажмите «Валидатор»");
            }
        });

    egui::Panel::bottom("status").show(&mut root, |ui| {
        ui.horizontal(|ui| {
            ui.label(&s.status);
            ui.separator();
            ui.label(format!(
                "элементов: {}  ревизия: {}",
                s.world().element_count(),
                s.world().revision().0
            ));
            if let Some(p) = &sync.playtest {
                ui.separator();
                ui.label(p);
            }
        });
    });
    Ok(())
}

/// Запустить настоящий сервер на копии карты (в потоке) и клиент (отдельным процессом, если он собран).
fn start_playtest(s: &StudioState) -> String {
    use astrocore_editor::playtest::{PlaytestConfig, start_playtest_tcp};
    let cfg = PlaytestConfig {
        floor: s.floor,
        spawn_cell: astrocore_math::GridCell::new(1, 1),
        tick_hz: 20,
    };
    let mut server = match start_playtest_tcp(s.world(), &cfg) {
        Ok(srv) => srv,
        Err(e) => return format!("Пробная игра не запустилась: {e}"),
    };
    let addr = server
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let world_id = s.world().id().to_string();
    std::thread::spawn(move || {
        let _ = server.run(Some(20 * 60 * 30)); // 30 минут
    });
    let client = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("astrocore-client")));
    match client.filter(|c| c.exists()) {
        Some(c) => match std::process::Command::new(c)
            .args(["--connect", &addr, "--world-id", &world_id])
            .spawn()
        {
            Ok(_) => format!("Пробная игра: сервер {addr}, клиент запущен"),
            Err(e) => format!("Сервер {addr}; клиент не запустился: {e}"),
        },
        None => format!("Пробная игра: сервер {addr} (соберите astrocore-client и подключитесь)"),
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
