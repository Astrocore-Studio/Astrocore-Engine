//! Окна интерфейса клиента на `bevy_ui`: рамка с заголовком, перетаскиванием и кнопкой
//! закрытия, содержимое рисует построитель вида окна. Окна открывает сервер
//! (`astrocore-ui`): слой следит за списком открытых интерфейсов мира игры.
//! Образец — `BoundUserInterface` и `DefaultWindow` Robust.

use std::collections::HashMap;
use std::sync::Arc;

use astrocore_ui::{OpenUi, OpenUis, UiWorldExt};
use bevy::prelude::*;
// Наведение и нажатие узлов: прежняя система фокуса `bevy_ui`, она работает без подключения
// выбора сущностей (`bevy_picking`).
use bevy::ui::{DeprecatedInteraction as Interaction, FocusPolicy};
use bevy::window::PrimaryWindow;

use crate::Game;
use crate::view::{GameResources, entity_label};

/// Шрифт интерфейса с кириллицей из ресурсов игры.
const FONT_PATH: &str = "Fonts/NotoSans/NotoSans-Regular.ttf";

const FRAME: Color = Color::srgb(0.11, 0.12, 0.15);
const TITLE: Color = Color::srgb(0.17, 0.19, 0.25);
const BORDER: Color = Color::srgb(0.30, 0.33, 0.42);
const BUTTON: Color = Color::srgb(0.24, 0.27, 0.36);
const TEXT: Color = Color::srgb(0.90, 0.91, 0.94);

/// Шрифт и размеры текста интерфейса.
#[derive(Resource, Clone, Default)]
pub struct UiStyle {
    pub font: Handle<Font>,
}

impl UiStyle {
    /// Текст интерфейса заданного размера.
    pub fn text(&self, text: impl Into<String>, size: f32) -> impl Bundle {
        (
            Text::new(text),
            TextFont {
                font: self.font.clone().into(),
                font_size: size.into(),
                ..default()
            },
            TextColor(TEXT),
        )
    }
}

/// Действие кнопки окна: получает мир игры, например чтобы отправить серверу сообщение
/// окна (`send_ui_message`).
#[derive(Component, Clone)]
pub struct UiAction(pub Arc<dyn Fn(&mut bevy_ecs::world::World) + Send + Sync>);

impl UiAction {
    pub fn new(action: impl Fn(&mut bevy_ecs::world::World) + Send + Sync + 'static) -> Self {
        Self(Arc::new(action))
    }
}

/// Построитель содержимого окна: создаёт дочерние узлы узла содержимого по состоянию
/// интерфейса. Вызывается при открытии окна и при каждом новом состоянии.
pub type WindowBuilder =
    Arc<dyn Fn(&mut Commands, Entity, &OpenUi, &bevy_ecs::world::World, &UiStyle) + Send + Sync>;

/// Построители окон по виду интерфейса (`type` в компоненте `UserInterface`).
#[derive(Resource, Default)]
pub struct WindowKinds(HashMap<String, WindowBuilder>);

impl WindowKinds {
    /// Назначает построитель виду окна.
    pub fn register(
        &mut self,
        kind: &str,
        builder: impl Fn(&mut Commands, Entity, &OpenUi, &bevy_ecs::world::World, &UiStyle)
        + Send
        + Sync
        + 'static,
    ) {
        self.0.insert(kind.to_owned(), Arc::new(builder));
    }
}

/// Курсор над окном интерфейса: нажатия мыши в этот момент мир не получает.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct UiHover(pub bool);

/// Рамка окна открытого интерфейса.
#[derive(Component)]
pub(crate) struct WindowFrame {
    entity: bevy_ecs::entity::Entity,
    key: String,
    content: Entity,
    /// Состояние, по которому построено содержимое.
    built: Option<Option<Vec<u8>>>,
}

#[derive(Component)]
pub(crate) struct TitleBar(Entity);

#[derive(Component)]
pub(crate) struct CloseButton(Entity);

/// Кнопка с текстом и действием внутри узла.
pub fn spawn_button(
    commands: &mut Commands,
    parent: Entity,
    style: &UiStyle,
    text: &str,
    action: UiAction,
) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            Interaction::default(),
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                margin: UiRect::top(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
            action,
            children![style.text(text, 14.0)],
        ))
        .id()
}

/// Строка текста внутри узла.
pub fn spawn_label(commands: &mut Commands, parent: Entity, style: &UiStyle, text: &str) -> Entity {
    commands
        .spawn((ChildOf(parent), style.text(text, 14.0)))
        .id()
}

pub(crate) fn load_font(
    resources: Option<Res<GameResources>>,
    mut fonts: ResMut<Assets<Font>>,
    mut style: ResMut<UiStyle>,
) {
    let loaded = resources
        .and_then(|resources| resources.0.read(FONT_PATH).ok())
        .map(Font::from_bytes);
    if let Some(font) = loaded {
        style.font = fonts.add(font);
    }
}

/// Создаёт, обновляет и убирает окна по списку открытых интерфейсов мира игры.
pub(crate) fn sync_windows(
    mut commands: Commands,
    game: Res<Game>,
    kinds: Res<WindowKinds>,
    style: Res<UiStyle>,
    mut seen: Local<Option<u64>>,
    mut frames: Query<(Entity, &mut WindowFrame)>,
) {
    let world = &game.0;
    let Some(open) = world.get_resource::<OpenUis>() else {
        return;
    };
    if *seen == Some(open.revision) {
        return;
    }
    *seen = Some(open.revision);

    let mut shown: Vec<(bevy_ecs::entity::Entity, String)> = Vec::new();
    for (frame_entity, mut frame) in &mut frames {
        let Some(ui) = open
            .list
            .iter()
            .find(|ui| ui.entity == frame.entity && ui.key == frame.key)
        else {
            commands.entity(frame_entity).despawn();
            continue;
        };
        shown.push((ui.entity, ui.key.clone()));
        if frame.built.as_ref() != Some(&ui.state) {
            frame.built = Some(ui.state.clone());
            commands.entity(frame.content).despawn_related::<Children>();
            build_content(&mut commands, frame.content, ui, world, &kinds, &style);
        }
    }
    for (index, ui) in open.list.iter().enumerate() {
        if shown
            .iter()
            .any(|(entity, key)| *entity == ui.entity && *key == ui.key)
        {
            continue;
        }
        // Новые окна ложатся лесенкой, чтобы не закрывать друг друга целиком.
        let shift = 28.0 * index as f32;
        let frame = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(140.0 + shift),
                    top: Val::Px(110.0 + shift),
                    min_width: Val::Px(280.0),
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(FRAME),
                BorderColor::all(BORDER),
                Interaction::default(),
                FocusPolicy::Block,
            ))
            .id();
        commands.spawn((
            ChildOf(frame),
            Node {
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                column_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(TITLE),
            Interaction::default(),
            TitleBar(frame),
            children![
                style.text(entity_label(world, ui.entity), 15.0),
                (
                    Interaction::default(),
                    Node {
                        padding: UiRect::axes(Val::Px(7.0), Val::Px(0.0)),
                        ..default()
                    },
                    BackgroundColor(BUTTON),
                    CloseButton(frame),
                    children![style.text("×", 16.0)],
                ),
            ],
        ));
        let content = commands
            .spawn((
                ChildOf(frame),
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::FlexStart,
                    padding: UiRect::all(Val::Px(10.0)),
                    ..default()
                },
            ))
            .id();
        build_content(&mut commands, content, ui, world, &kinds, &style);
        commands.entity(frame).insert(WindowFrame {
            entity: ui.entity,
            key: ui.key.clone(),
            content,
            built: Some(ui.state.clone()),
        });
    }
}

fn build_content(
    commands: &mut Commands,
    content: Entity,
    ui: &OpenUi,
    world: &bevy_ecs::world::World,
    kinds: &WindowKinds,
    style: &UiStyle,
) {
    match kinds.0.get(&ui.kind) {
        Some(builder) => builder(commands, content, ui, world, style),
        None => {
            let text = format!("Окно «{}» ещё не перенесено из SS14.", ui.kind);
            spawn_label(commands, content, style, &text);
        }
    }
}

/// Кнопки окон: закрытие и действия; перетаскивание окна за заголовок.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn window_input(
    mut game: ResMut<Game>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    mut hover: ResMut<UiHover>,
    mut drag: Local<Option<(Entity, Vec2)>>,
    closes: Query<(&Interaction, &CloseButton), Changed<Interaction>>,
    actions: Query<(&Interaction, &UiAction), Changed<Interaction>>,
    titles: Query<(&Interaction, &TitleBar)>,
    blocking: Query<&Interaction, With<Node>>,
    mut frames: Query<(&mut Node, &WindowFrame)>,
) {
    hover.0 = blocking
        .iter()
        .any(|interaction| *interaction != Interaction::None);
    for (interaction, close) in &closes {
        if *interaction == Interaction::Pressed
            && let Ok((_, frame)) = frames.get(close.0)
        {
            game.0.request_close_ui(frame.entity, &frame.key);
        }
    }
    for (interaction, action) in &actions {
        if *interaction == Interaction::Pressed {
            (action.0)(&mut game.0);
        }
    }
    // Перетаскивание: пока заголовок нажат, окно держит своё смещение от курсора.
    let cursor = window.and_then(|window| window.cursor_position());
    let pressed = titles
        .iter()
        .find(|(interaction, _)| **interaction == Interaction::Pressed)
        .map(|(_, title)| title.0);
    match (pressed, cursor) {
        (Some(frame), Some(cursor)) => {
            let Ok((mut node, _)) = frames.get_mut(frame) else {
                return;
            };
            let corner = match (node.left, node.top) {
                (Val::Px(left), Val::Px(top)) => Vec2::new(left, top),
                _ => Vec2::ZERO,
            };
            let grab = match *drag {
                Some((dragged, grab)) if dragged == frame => grab,
                _ => cursor - corner,
            };
            *drag = Some((frame, grab));
            let target = (cursor - grab).max(Vec2::ZERO);
            if target != corner {
                node.left = Val::Px(target.x);
                node.top = Val::Px(target.y);
            }
        }
        _ => *drag = None,
    }
}

/// Подключает окна к приложению клиента.
pub(crate) fn build(app: &mut App) {
    app.init_resource::<UiStyle>()
        .init_resource::<WindowKinds>()
        .init_resource::<UiHover>()
        .add_systems(Startup, load_font)
        .add_systems(Update, (window_input, sync_windows).chain());
}
