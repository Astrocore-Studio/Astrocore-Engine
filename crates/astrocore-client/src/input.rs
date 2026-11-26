//! Ввод клиента: клавиши и мышь окна → функции ввода (`astrocore-input`), курсор в мире и
//! сущность под курсором. Образец — `ClydeInputManager`, `InputSystem` и `ClickableSystem`
//! Robust.

use astrocore_input::{
    BoundKeyState, Clickable, FALLBACK_KEYBINDS, FunctionChange, InputContexts, InputWorldExt,
    KeyBindings, engine_contexts, functions,
};
use astrocore_movement::{LocalInput, MoveButtons};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::Game;
use crate::view::{EntityView, Views};

/// Назначения клавиш и контексты ввода клиента. Игра вставляет ресурс со своим
/// `keybinds.yml`; без него действуют назначения движения по умолчанию.
#[derive(Resource)]
pub struct Bindings {
    pub keys: KeyBindings,
    pub contexts: InputContexts,
}

impl Default for Bindings {
    fn default() -> Self {
        let mut keys = KeyBindings::default();
        if let Ok(node) = astrocore_data::parse_yaml(FALLBACK_KEYBINDS) {
            // Встроенный текст разбирается всегда; при ошибке остаётся пустой набор.
            let _ = keys.load_defaults(&node);
        }
        let mut contexts = engine_contexts();
        contexts.set_active("human");
        Self { keys, contexts }
    }
}

/// Курсор в мире игры: точка на карте глаза.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Cursor {
    pub world: Option<Vec2>,
}

/// Имя клавиши в Robust (`Keyboard.Key`).
pub fn key_name(key: KeyCode) -> Option<String> {
    let special = match key {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "Control",
        KeyCode::AltLeft | KeyCode::AltRight => "Alt",
        KeyCode::SuperLeft => "LSystem",
        KeyCode::SuperRight => "RSystem",
        KeyCode::ContextMenu => "Menu",
        KeyCode::Enter => "Return",
        KeyCode::Backspace => "BackSpace",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::BracketLeft => "LBracket",
        KeyCode::BracketRight => "RBracket",
        KeyCode::Semicolon => "SemiColon",
        KeyCode::Quote => "Apostrophe",
        KeyCode::Backquote => "Tilde",
        KeyCode::Backslash => "BackSlash",
        KeyCode::NumpadAdd => "NumpadAdd",
        KeyCode::NumpadSubtract => "NumpadSubtract",
        KeyCode::NumpadMultiply => "NumpadMultiply",
        KeyCode::NumpadDivide => "NumpadDivide",
        KeyCode::NumpadDecimal => "NumpadDecimal",
        KeyCode::NumpadEnter => "NumpadEnter",
        _ => "",
    };
    if !special.is_empty() {
        return Some(special.to_owned());
    }
    // Остальные имена получаются из имён Bevy: `KeyW` → `W`, `Digit1` → `Num1`,
    // `Numpad1` → `NumpadNum1`; `F3`, `Escape`, `Space`, `Tab` и подобные совпадают.
    let name = format!("{key:?}");
    if let Some(letter) = name.strip_prefix("Key") {
        return Some(letter.to_owned());
    }
    if let Some(digit) = name.strip_prefix("Digit") {
        return Some(format!("Num{digit}"));
    }
    if let Some(digit) = name.strip_prefix("Numpad") {
        return Some(format!("NumpadNum{digit}"));
    }
    name.chars().all(char::is_alphanumeric).then_some(name)
}

/// Имя кнопки мыши в Robust.
pub fn mouse_name(button: MouseButton) -> Option<String> {
    Some(match button {
        MouseButton::Left => "MouseLeft".to_owned(),
        MouseButton::Right => "MouseRight".to_owned(),
        MouseButton::Middle => "MouseMiddle".to_owned(),
        MouseButton::Back => "MouseButton4".to_owned(),
        MouseButton::Forward => "MouseButton5".to_owned(),
        MouseButton::Other(number) => format!("MouseButton{number}"),
    })
}

/// Курсор окна → точка мира.
pub(crate) fn track_cursor(
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    camera: Single<(&Camera, &GlobalTransform), With<Camera2d>>,
    mut cursor: ResMut<Cursor>,
) {
    let (camera, transform) = *camera;
    cursor.world = window
        .and_then(|window| window.cursor_position())
        .and_then(|position| camera.viewport_to_world_2d(transform, position).ok());
}

/// Доля непрозрачности пикселя, с которой он считается частью спрайта (`ClickMap` Robust).
const CLICK_ALPHA: u8 = 64;

/// Сущность игры под точкой мира: верхняя из тех, у кого в этой точке непрозрачный пиксель
/// спрайта. Выбираются только сущности с компонентом `Clickable`.
#[allow(clippy::type_complexity)]
pub(crate) fn entity_at(
    point: Vec2,
    game: &bevy_ecs::world::World,
    views: &Views,
    shown: &Query<(&Transform, &Children), With<EntityView>>,
    layers: &Query<(&Transform, &Sprite), Without<EntityView>>,
    images: &Assets<Image>,
    layouts: &Assets<TextureAtlasLayout>,
) -> Option<bevy_ecs::entity::Entity> {
    entities_at(point, game, views, shown, layers, images, layouts)
        .into_iter()
        .next()
}

/// Все сущности с `Clickable` под точкой, сверху вниз (`GetClickableEntities` Robust).
pub(crate) fn entities_at(
    point: Vec2,
    game: &bevy_ecs::world::World,
    views: &Views,
    shown: &Query<(&Transform, &Children), With<EntityView>>,
    layers: &Query<(&Transform, &Sprite), Without<EntityView>>,
    images: &Assets<Image>,
    layouts: &Assets<TextureAtlasLayout>,
) -> Vec<bevy_ecs::entity::Entity> {
    let mut hits: Vec<(f32, bevy_ecs::entity::Entity)> = Vec::new();
    for (entity, view) in views.iter() {
        if game.get::<Clickable>(entity).is_none() {
            continue;
        }
        let Ok((transform, children)) = shown.get(view) else {
            continue;
        };
        // Спрайты крупнее нескольких тайлов редки: дальние сущности отсекаются сразу.
        if transform.translation.truncate().distance_squared(point) > 16.0 {
            continue;
        }
        let mut top: Option<f32> = None;
        for child in children.iter() {
            let Ok((layer, sprite)) = layers.get(child) else {
                continue;
            };
            let depth = transform.translation.z + layer.translation.z;
            if top.is_some_and(|best| best >= depth) {
                continue;
            }
            if opaque_at(point, transform, layer, sprite, images, layouts) {
                top = Some(depth);
            }
        }
        if let Some(depth) = top {
            hits.push((depth, entity));
        }
    }
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits.into_iter().map(|(_, entity)| entity).collect()
}

/// Сущности под курсором сверху вниз: по ним игра строит контекстное меню.
#[derive(Resource, Debug, Default, Clone)]
pub struct Hovered {
    pub entities: Vec<bevy_ecs::entity::Entity>,
}

/// Обновляет [`Hovered`] по курсору.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn track_hovered(
    cursor: Res<Cursor>,
    game: Res<Game>,
    views: Res<Views>,
    images: Res<Assets<Image>>,
    layouts: Res<Assets<TextureAtlasLayout>>,
    shown: Query<(&Transform, &Children), With<EntityView>>,
    layers: Query<(&Transform, &Sprite), Without<EntityView>>,
    mut hovered: ResMut<Hovered>,
) {
    let entities = cursor
        .world
        .map(|point| entities_at(point, &game.0, &views, &shown, &layers, &images, &layouts))
        .unwrap_or_default();
    if hovered.entities != entities {
        hovered.entities = entities;
    }
}

fn opaque_at(
    point: Vec2,
    parent: &Transform,
    layer: &Transform,
    sprite: &Sprite,
    images: &Assets<Image>,
    layouts: &Assets<TextureAtlasLayout>,
) -> bool {
    let (Some(size), Some(image)) = (sprite.custom_size, images.get(&sprite.image)) else {
        return false;
    };
    if sprite.color.alpha() <= 0.0 || size.x <= 0.0 || size.y <= 0.0 {
        return false;
    }
    let to_local = (parent.to_matrix() * layer.to_matrix()).inverse();
    let local = to_local.transform_point3(point.extend(0.0)).truncate();
    // Доли ширины и высоты картинки: (0, 0) — левый верхний угол.
    let uv = Vec2::new(local.x / size.x + 0.5, 0.5 - local.y / size.y);
    if !(0.0..1.0).contains(&uv.x) || !(0.0..1.0).contains(&uv.y) {
        return false;
    }
    let rect = sprite
        .texture_atlas
        .as_ref()
        .and_then(|atlas| {
            layouts
                .get(&atlas.layout)?
                .textures
                .get(atlas.index)
                .copied()
        })
        .unwrap_or_else(|| URect::new(0, 0, image.width(), image.height()));
    let x = rect.min.x + (uv.x * rect.width() as f32) as u32;
    let y = rect.min.y + (uv.y * rect.height() as f32) as u32;
    let index = (y as usize * image.width() as usize + x as usize) * 4 + 3;
    image
        .data
        .as_ref()
        .and_then(|data| data.get(index))
        .is_some_and(|alpha| *alpha >= CLICK_ALPHA)
}

fn move_button(function: &str) -> Option<MoveButtons> {
    Some(match function {
        functions::MOVE_UP => MoveButtons::UP,
        functions::MOVE_DOWN => MoveButtons::DOWN,
        functions::MOVE_LEFT => MoveButtons::LEFT,
        functions::MOVE_RIGHT => MoveButtons::RIGHT,
        functions::WALK => MoveButtons::WALK,
        _ => return None,
    })
}

/// Нажатия окна → функции ввода: движение держит кнопки в `LocalInput`, остальные функции
/// уходят обработчикам клиента и серверу вместе с точкой и сущностью под курсором.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn read_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    cursor: Res<Cursor>,
    views: Res<Views>,
    images: Res<Assets<Image>>,
    layouts: Res<Assets<TextureAtlasLayout>>,
    shown: Query<(&Transform, &Children), With<EntityView>>,
    layers: Query<(&Transform, &Sprite), Without<EntityView>>,
    ui_hover: Res<crate::windows::UiHover>,
    mut bindings: ResMut<Bindings>,
    mut game: ResMut<Game>,
) {
    let bindings = &mut *bindings;
    let mut changes: Vec<FunctionChange> = Vec::new();
    // Окно без фокуса не получает отпусканий: всё нажатое отпускается.
    if window.is_some_and(|window| !window.focused) {
        changes.extend(bindings.keys.release_all());
    }
    let released = keys
        .get_just_released()
        .filter_map(|key| key_name(*key))
        .chain(mouse.get_just_released().filter_map(|b| mouse_name(*b)));
    for key in released {
        changes.extend(bindings.keys.key_up(&key));
    }
    let pressed = keys
        .get_just_pressed()
        .filter_map(|key| key_name(*key))
        .chain(mouse.get_just_pressed().filter_map(|b| mouse_name(*b)));
    for key in pressed {
        changes.extend(bindings.keys.key_down(&key, &bindings.contexts));
    }
    if changes.is_empty() {
        return;
    }

    // Сущность под курсором ищется один раз и только если она нужна.
    let mut target: Option<Option<bevy_ecs::entity::Entity>> = None;
    let mut handled_down = false;
    for change in changes {
        if let Some(button) = move_button(&change.function) {
            if let Some(mut input) = game.0.get_resource_mut::<LocalInput>() {
                input.buttons = match change.state {
                    BoundKeyState::Down => input.buttons.union(button),
                    BoundKeyState::Up => input.buttons.difference(button),
                };
            }
            continue;
        }
        // Нажатие, обработанное функцией с большим приоритетом, до остальных не доходит;
        // нажатие мыши над окном интерфейса получает окно.
        if change.state == BoundKeyState::Down && (handled_down || (change.can_focus && ui_hover.0))
        {
            continue;
        }
        let under = *target.get_or_insert_with(|| {
            cursor.world.and_then(|point| {
                entity_at(point, &game.0, &views, &shown, &layers, &images, &layouts)
            })
        });
        let handled = game
            .0
            .send_input(&change.function, change.state, cursor.world, under);
        handled_down |= handled && change.state == BoundKeyState::Down;
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    use super::*;

    #[test]
    fn window_keys_get_robust_names() {
        assert_eq!(key_name(KeyCode::KeyW).as_deref(), Some("W"));
        assert_eq!(key_name(KeyCode::Digit1).as_deref(), Some("Num1"));
        assert_eq!(key_name(KeyCode::Numpad7).as_deref(), Some("NumpadNum7"));
        assert_eq!(key_name(KeyCode::ShiftRight).as_deref(), Some("Shift"));
        assert_eq!(key_name(KeyCode::F3).as_deref(), Some("F3"));
        assert_eq!(key_name(KeyCode::Escape).as_deref(), Some("Escape"));
        assert_eq!(key_name(KeyCode::Space).as_deref(), Some("Space"));
        assert_eq!(key_name(KeyCode::Enter).as_deref(), Some("Return"));
        assert_eq!(mouse_name(MouseButton::Left).as_deref(), Some("MouseLeft"));
    }

    #[test]
    fn click_hits_only_opaque_pixels() {
        // Картинка 2×2: непрозрачен только левый верхний пиксель.
        let mut data = vec![0u8; 16];
        data[3] = 255;
        let image = Image::new(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        let mut images = Assets::<Image>::default();
        let layouts = Assets::<TextureAtlasLayout>::default();
        let sprite = Sprite {
            image: images.add(image),
            custom_size: Some(Vec2::ONE),
            ..default()
        };
        // Сущность стоит в точке (10, 5); спрайт занимает тайл вокруг неё.
        let parent = Transform::from_xyz(10.0, 5.0, 0.0);
        let layer = Transform::IDENTITY;
        let hit = |x: f32, y: f32, parent: &Transform| {
            opaque_at(Vec2::new(x, y), parent, &layer, &sprite, &images, &layouts)
        };
        assert!(hit(9.75, 5.25, &parent));
        assert!(!hit(10.25, 5.25, &parent));
        assert!(!hit(9.75, 4.75, &parent));
        assert!(!hit(8.0, 5.0, &parent));
        // Повёрнутая на четверть оборота сущность: непрозрачный угол уходит влево вниз.
        let turned = parent.with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        assert!(hit(9.75, 4.75, &turned));
        assert!(!hit(9.75, 5.25, &turned));
    }
}
