//! Графический клиент Astrocore поверх Bevy: мир игры в ресурсе [`Game`], такт с частотой
//! сервера, отрисовка спрайтов RSI и тайлов, камера за игроком, ввод с клавиатуры.
//! Образец — `Robust.Client` (спрайты, глаз, ввод).

mod audio;
mod input;
mod lighting;
mod sprite;
mod textures;
mod view;
mod visualizer;
mod windows;

use std::path::PathBuf;
use std::time::Duration;

use astrocore_ecs::{GameWorldExt, RegistryWorldExt};
use astrocore_net::{ClientNet, ClientTransport};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

pub use audio::MasterVolume;
pub use input::{Bindings, Cursor, Hovered, key_name, mouse_name};
pub use lighting::{Lighting, TEXELS_PER_TILE};
pub use sprite::{DrawDepth, LayerSource, ResolvedLayer, Sprite, SpriteLayer, register_draw_depth};
pub use textures::{PIXELS_PER_TILE, StateTexture, Textures, rsi_direction};
pub use view::{Eye, GameResources, VIEW_TILES, Views, entity_label};
pub use visualizer::{GenericVisualizer, SpriteLayerData};
pub use windows::{Tooltip, UiAction, UiHover, UiStyle, WindowKinds, spawn_button, spawn_label};

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
    world.add_plugin(astrocore_input::InputPlugin);
    world.add_plugin(astrocore_audio::AudioPlugin);
    world.add_plugin(astrocore_light::LightPlugin);
    world.add_plugin(astrocore_ui::UiPlugin);
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
        lighting::build(app);
        windows::build(app);
        app.init_resource::<Views>()
            .init_resource::<Bindings>()
            .init_resource::<MasterVolume>()
            .init_resource::<audio::Sounds>()
            .init_resource::<audio::AmbientPlaying>()
            .init_resource::<Cursor>()
            .init_resource::<Hovered>()
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
                    input::track_cursor,
                    input::track_hovered,
                    input::read_input,
                    view::follow_player,
                    view::sync_chunks,
                    view::sync_entities,
                    view::follow_view,
                    lighting::update_lighting,
                    view::animate_layers,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    audio::play_queued,
                    audio::update_ambient,
                    audio::update_emitters,
                )
                    .chain()
                    .after(view::follow_player),
            )
            .add_systems(
                Update,
                capture_frame
                    .after(view::animate_layers)
                    .run_if(resource_exists::<Capture>),
            );
    }
}
