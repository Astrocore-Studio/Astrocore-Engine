//! Графический клиент Astrocore поверх Bevy: мир игры в ресурсе [`Game`], такт с частотой
//! сервера, отрисовка спрайтов RSI и тайлов, камера за игроком, ввод с клавиатуры.
//! Образец — `Robust.Client` (спрайты, глаз, ввод).

mod sprite;
mod textures;
mod view;
mod visualizer;

use std::path::PathBuf;
use std::time::Duration;

use astrocore_ecs::{GameWorldExt, RegistryWorldExt};
use astrocore_movement::{LocalInput, MoveButtons};
use astrocore_net::{ClientNet, ClientTransport};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

pub use sprite::{DrawDepth, LayerSource, ResolvedLayer, Sprite, SpriteLayer, register_draw_depth};
pub use textures::{PIXELS_PER_TILE, StateTexture, Textures, rsi_direction};
pub use view::{Eye, GameResources, VIEW_TILES, Views, entity_label};
pub use visualizer::{GenericVisualizer, SpriteLayerData};

/// Мир игры клиента. Ресурс приложения Bevy.
#[derive(Resource)]
pub struct Game(pub bevy_ecs::world::World);

/// Транспорт клиента. Хранится вне потоков: соединение Lightyear не переносится между ними.
pub struct Link(pub Box<dyn ClientTransport>);

/// Снять кадр в файл и завершить приложение: проверка отрисовки без человека.
#[derive(Resource, Debug, Clone)]
pub struct Capture {
    pub path: PathBuf,
    /// Сколько кадров отрисовать до снимка.
    pub after_frames: u32,
}

/// Регистрирует в мире игры компоненты данных клиента и системы внешнего вида.
pub fn register_client_components(world: &mut bevy_ecs::world::World) {
    world.register_data_component::<Sprite>();
    visualizer::install(world);
}

/// Такт мира игры: приём пакетов, такт, отправка. Идёт с частотой сервера.
fn tick(mut game: ResMut<Game>, mut link: NonSendMut<Link>, mut fixed: ResMut<Time<Fixed>>) {
    link.0.receive(&mut game.0);
    game.0.run_tick();
    link.0.send(&mut game.0);
    // Частота тактов приходит от сервера в приветствии.
    let rate = game.0.resource::<ClientNet>().tick_rate;
    if rate > 0 {
        let period = Duration::from_secs_f64(1.0 / f64::from(rate));
        if fixed.timestep() != period {
            fixed.set_timestep(period);
        }
    }
}

/// Клавиши движения: WASD и стрелки, Shift — шаг вместо бега.
fn read_keyboard(keys: Res<ButtonInput<KeyCode>>, mut game: ResMut<Game>) {
    let held = |a: KeyCode, b: KeyCode| keys.pressed(a) || keys.pressed(b);
    let mut buttons = MoveButtons::NONE;
    for (first, second, button) in [
        (KeyCode::KeyW, KeyCode::ArrowUp, MoveButtons::UP),
        (KeyCode::KeyS, KeyCode::ArrowDown, MoveButtons::DOWN),
        (KeyCode::KeyA, KeyCode::ArrowLeft, MoveButtons::LEFT),
        (KeyCode::KeyD, KeyCode::ArrowRight, MoveButtons::RIGHT),
        (KeyCode::ShiftLeft, KeyCode::ShiftRight, MoveButtons::WALK),
    ] {
        if held(first, second) {
            buttons = buttons.union(button);
        }
    }
    if let Some(mut input) = game.0.get_resource_mut::<LocalInput>()
        && input.buttons != buttons
    {
        input.buttons = buttons;
    }
}

fn capture_frame(
    mut commands: Commands,
    capture: Res<Capture>,
    views: Res<Views>,
    mut frames: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    // Отсчёт начинается, когда мир получен и нарисован.
    if views.entity_count() == 0 {
        return;
    }
    *frames += 1;
    if *frames == capture.after_frames {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(capture.path.clone()));
    }
    if *frames > capture.after_frames + 10 && capture.path.is_file() {
        exit.write(AppExit::Success);
    }
}

/// Клиент игры: такт мира, отрисовка, камера и ввод. Перед запуском приложение вставляет
/// ресурсы [`Game`], [`GameResources`] и транспорт [`Link`] (`insert_non_send`).
pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Views>()
            .init_resource::<Textures>()
            .init_resource::<Eye>()
            .insert_resource(Time::<Fixed>::from_hz(f64::from(
                astrocore_ecs::DEFAULT_TICK_RATE,
            )))
            .add_systems(Startup, view::spawn_camera)
            .add_systems(FixedUpdate, tick)
            .add_systems(
                Update,
                (
                    read_keyboard,
                    view::follow_player,
                    view::sync_chunks,
                    view::sync_entities,
                    view::follow_view,
                    view::animate_layers,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                capture_frame
                    .after(view::animate_layers)
                    .run_if(resource_exists::<Capture>),
            );
    }
}
