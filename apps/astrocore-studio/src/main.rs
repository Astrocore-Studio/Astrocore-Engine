use std::path::PathBuf;

use astrocore_editor::gizmo::{Axis, Ray};
use astrocore_editor::view::FloorView;
use astrocore_render::{
    BillboardPlugin, CameraMode, CameraRig, CameraRigPlugin, RenderDiagnosticsPlugin, RenderStats,
    RenderedWorld, WorldMeshPlugin,
};
use astrocore_studio::{GIZMO_LENGTH, Inspector, Pointer, Preview, StudioState, Tool};
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
    panels: bool,
    overlays: bool,
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
    show_console: bool,
    show_diff: bool,
    show_profiler: bool,
    /// Редактируемая копия свойств выбранного элемента (применяется по кнопке).
    draft: Option<Inspector>,
    prefab_offset: [i32; 2],
    prefab_turns: u8,
}

fn main() {
    let mut args = Args {
        screenshot: None,
        frames: 30,
        demo_room: false,
        panels: false,
        overlays: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--frames" => args.frames = it.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--demo-room" => args.demo_room = true,
            "--panels" => args.panels = true,
            "--overlays" => args.overlays = true,
            "-h" | "--help" => {
                eprintln!(
                    "astrocore-studio [--screenshot файл.png [--frames N]] [--demo-room] [--panels]"
                );
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
    if args.overlays {
        state.overlays = astrocore_studio::Overlays {
            collision: true,
            topology: true,
            wireframe: false,
        };
    }
    let world = state.world().clone();
    let panels = args.panels;

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
        .add_plugins((
            WorldMeshPlugin,
            BillboardPlugin,
            CameraRigPlugin,
            RenderDiagnosticsPlugin,
        ))
        .insert_resource(ClearColor(Color::srgb(0.18, 0.2, 0.24)))
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 500.0,
            ..default()
        })
        .insert_resource(RenderedWorld::new(world))
        .insert_resource(Studio(state))
        .insert_resource(args)
        .insert_resource(SyncState {
            show_console: panels,
            show_diff: panels,
            show_profiler: panels,
            ..default()
        })
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
    // Свободная модель с выбором: на скриншоте видны гизмо и панель свойств.
    s.tool = Tool::Model;
    s.model = "studio:statue".into();
    drag(s, p(9.5, 3.0), p(9.5, 3.0));
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
    mut rotating: Local<bool>,
) {
    let over_ui = egui_wants_pointer(&mut contexts);
    let (Ok(window), Ok((camera, transform))) = (windows.single(), cameras.single()) else {
        return;
    };
    studio.0.additive_select =
        keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    // Луч из камеры через курсор: гизмо работает в пространстве, а не на плоскости этажа.
    let ray = window
        .cursor_position()
        .and_then(|c| camera.viewport_to_world(transform, c).ok())
        .map(|r| Ray {
            origin: r.origin,
            dir: r.direction.into(),
        });
    if let Some(ray) = ray {
        let snap = if keys.pressed(KeyCode::AltLeft) {
            0.0
        } else {
            studio.0.gizmo_snap
        };
        if mouse.just_pressed(MouseButton::Left) && !over_ui {
            if keys.pressed(KeyCode::KeyR) && studio.0.gizmo_origin().is_some() {
                *rotating = true;
                return;
            }
            if studio.0.tool == Tool::Select && studio.0.gizmo_press(ray) {
                return;
            }
        }
        if studio.0.gizmo_axis().is_some() {
            if mouse.pressed(MouseButton::Left) {
                studio.0.gizmo_drag(ray, snap);
            }
            if mouse.just_released(MouseButton::Left) {
                studio.0.gizmo_release();
            }
            return;
        }
        if *rotating {
            if mouse.just_released(MouseButton::Left) {
                studio.0.rotate_selected_toward(ray, 15.0);
                *rotating = false;
            }
            return;
        }
    }

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
fn draw_helpers(mut gizmos: Gizmos, mut studio: ResMut<Studio>, rigs: Query<&CameraRig>) {
    let s = &mut studio.0;
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
    if s.overlays.collision || s.overlays.topology || s.overlays.wireframe {
        let g = s.overlay_geometry().clone();
        for (lo, hi) in &g.collision {
            let center = (*lo + *hi) * 0.5;
            let size = (*hi - *lo).max(Vec3::splat(0.02));
            gizmos.cube(
                Transform::from_translation(center).with_scale(size),
                Color::srgba(1.0, 0.4, 0.2, 0.8),
            );
        }
        for (room, min, size) in &g.rooms {
            let hue = (*room as f32 * 47.0) % 360.0;
            let (a, b) = (*min + Vec3::Y * 0.06, *min + Vec3::new(*size, 0.06, *size));
            let c = Color::hsl(hue, 0.8, 0.55);
            let (p, q) = (Vec3::new(a.x, a.y, b.z), Vec3::new(b.x, a.y, a.z));
            for (u, v) in [(a, p), (p, b), (b, q), (q, a)] {
                gizmos.line(u, v, c);
            }
        }
        for (a, b) in &g.wire {
            gizmos.line(*a, *b, Color::srgba(0.8, 0.9, 1.0, 0.5));
        }
    }
    let s = &studio.0;
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
    if let Some(origin) = s.gizmo_origin() {
        let active = s.gizmo_axis();
        for (axis, color) in [
            (Axis::X, Color::srgb(0.95, 0.25, 0.25)),
            (Axis::Y, Color::srgb(0.3, 0.9, 0.3)),
            (Axis::Z, Color::srgb(0.3, 0.45, 1.0)),
        ] {
            let d = axis.dir();
            let dir = Vec3::new(d.x, d.y, d.z);
            let color = if active == Some(axis) {
                Color::srgb(1.0, 0.95, 0.3)
            } else {
                color
            };
            let (o, tip) = (Vec3::new(origin.x, origin.y, origin.z), dir * GIZMO_LENGTH);
            gizmos.line(o, o + tip, color);
            gizmos.sphere(Isometry3d::from_translation(o + tip), 0.08, color);
        }
        if let Some(g) = s.gizmo_ghost() {
            gizmos.sphere(
                Isometry3d::from_translation(Vec3::new(g.x, g.y + 0.05, g.z)),
                0.25,
                ghost,
            );
        }
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
    stats: Res<RenderStats>,
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
            ui.menu_button("Окна", |ui| {
                ui.checkbox(&mut sync.show_console, "Консоль");
                ui.checkbox(&mut sync.show_diff, "Различия");
                ui.checkbox(&mut sync.show_profiler, "Профилировщик");
                ui.separator();
                ui.checkbox(&mut s.overlays.collision, "Оверлей: коллизия");
                ui.checkbox(&mut s.overlays.topology, "Оверлей: комнаты");
                ui.checkbox(&mut s.overlays.wireframe, "Оверлей: каркас");
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
                Tool::Model => {
                    ui.label("Модель (ID, файл ns_имя.glb)");
                    ui.text_edit_singleline(&mut s.model);
                    for (i, name) in ["полуширина X", "полувысота", "полуширина Z"]
                        .into_iter()
                        .enumerate()
                    {
                        ui.add(egui::Slider::new(&mut s.model_half[i], 0.05..=10.0).text(name));
                    }
                }
                _ => {}
            }
            ui.add(egui::Slider::new(&mut s.gizmo_snap, 0.0..=2.0).text("привязка гизмо, м"));
            ui.small("Гизмо: тянуть ось у выбранной модели (Alt — без привязки); R + клик — повернуть к курсору");
            ui.separator();
            ui.heading("Префабы");
            let ids = s.prefab_ids();
            if ids.is_empty() {
                ui.small("В проекте нет префабов");
            }
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut sync.prefab_offset[0]).prefix("dx "));
                ui.add(egui::DragValue::new(&mut sync.prefab_offset[1]).prefix("dz "));
                ui.add(egui::DragValue::new(&mut sync.prefab_turns).range(0..=3).prefix("поворот "));
            });
            for id in ids {
                if ui.button(format!("＋ {id}")).clicked() {
                    let (dx, dz) = (sync.prefab_offset[0], sync.prefab_offset[1]);
                    s.place_prefab(&id, dx, dz, sync.prefab_turns);
                }
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
            match s.inspect() {
                Some(current) => {
                    if sync.draft.as_ref().map(|d| d.id) != Some(current.id) {
                        sync.draft = Some(current.clone());
                    }
                    if let Some(d) = sync.draft.as_mut() {
                        ui.label(format!("{} · {}", d.kind, d.id));
                        ui.label("Материал / модель");
                        ui.text_edit_singleline(&mut d.material);
                        ui.add(egui::Slider::new(&mut d.thickness, 0.02..=2.0).text("толщина"));
                        if let Some(f) = d.free.as_mut() {
                            for (i, name) in ["x", "y", "z"].into_iter().enumerate() {
                                ui.add(
                                    egui::DragValue::new(&mut f.position[i])
                                        .speed(0.05)
                                        .prefix(format!("{name} ")),
                                );
                            }
                            ui.add(
                                egui::Slider::new(&mut f.yaw_deg, -180.0..=180.0).text("поворот °"),
                            );
                            for (i, name) in ["½ X", "½ высота", "½ Z"].into_iter().enumerate()
                            {
                                ui.add(
                                    egui::DragValue::new(&mut f.half_m[i])
                                        .speed(0.05)
                                        .range(0.01..=100.0)
                                        .prefix(format!("{name} ")),
                                );
                            }
                        }
                        let (apply, reset) = ui
                            .horizontal(|ui| {
                                (
                                    ui.button("Применить").clicked(),
                                    ui.button("Сбросить").clicked(),
                                )
                            })
                            .inner;
                        if apply {
                            let draft = d.clone();
                            s.apply_inspector(&draft);
                        } else if reset {
                            *d = current.clone();
                        }
                    }
                }
                None => {
                    sync.draft = None;
                    let items: Vec<_> = s
                        .selection
                        .iter()
                        .filter_map(|id| {
                            s.world()
                                .element(*id)
                                .map(|e| (e.id, e.kind, e.material.clone()))
                        })
                        .collect();
                    for (id, kind, material) in items.iter().take(12) {
                        ui.small(format!("{kind:?} · {material} · {id}"));
                    }
                    if items.len() > 12 {
                        ui.label(format!("… и ещё {}", items.len() - 12));
                    }
                }
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

    if sync.show_console {
        egui::Window::new("Консоль")
            .default_size([520.0, 220.0])
            .show(&ctx, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &s.console {
                            ui.label(line);
                        }
                    });
            });
    }
    if sync.show_diff {
        egui::Window::new("Различия с сохранённым")
            .default_size([360.0, 160.0])
            .show(&ctx, |ui| match s.diff_since_save() {
                None => {
                    ui.label("Сохраните или откройте проект — тогда появится основа для сравнения");
                }
                Some(d) => {
                    ui.label(d.summary());
                    for (title, ids) in [
                        ("добавлено", &d.added),
                        ("удалено", &d.removed),
                        ("изменено", &d.modified),
                    ] {
                        if !ids.is_empty() {
                            ui.collapsing(format!("{title}: {}", ids.len()), |ui| {
                                for id in ids.iter().take(50) {
                                    ui.small(format!("{id}"));
                                }
                            });
                        }
                    }
                }
            });
    }
    if sync.show_profiler {
        egui::Window::new("Профилировщик")
            .default_size([420.0, 140.0])
            .show(&ctx, |ui| {
                ui.label(stats.summary());
                ui.small("Оценка по сцене Bevy; GPU-таймеров здесь нет (нужен реальный GPU).");
                ui.label(format!(
                    "правок в истории: отмена {} / повтор {}",
                    s.session.can_undo() as u8,
                    s.session.can_redo() as u8
                ));
            });
    }
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
