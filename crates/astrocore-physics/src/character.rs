//! Кинематический контроллер персонажа.
//!
//! Персонаж — вертикальная коробка (радиус × высота). Движение:
//! 1. горизонтальное перемещение по осям X и Z по очереди, мелкими подшагами (без «туннелирования»);
//! 2. при упоре в невысокое препятствие — подъём на ступеньку (`step_height`), если
//!    над ней достаточно места;
//! 3. вертикаль: гравитация, посадка на поверхность, удар головой о потолок.
//!
//! Контроллер детерминирован: тот же вход даёт тот же результат, поэтому клиент
//! может повторять серверное движение для предсказания (раздел 15).

use astrocore_math::Aabb;
use astrocore_math::glam::Vec3;

use crate::{BoxWorld, aabb_overlap};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterShape {
    pub radius: f32,
    pub height: f32,
    /// Максимальная высота, на которую персонаж поднимается без прыжка.
    pub step_height: f32,
}

impl Default for CharacterShape {
    fn default() -> Self {
        Self {
            radius: 0.3,
            height: 1.8,
            step_height: 0.35,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterState {
    /// Позиция ступней.
    pub pos: Vec3,
    pub vel_y: f32,
    pub grounded: bool,
}

impl CharacterState {
    pub fn at(pos: Vec3) -> Self {
        Self {
            pos,
            vel_y: 0.0,
            grounded: false,
        }
    }

    pub fn aabb(&self, shape: &CharacterShape) -> Aabb {
        character_box(self.pos, shape)
    }
}

pub const GRAVITY: f32 = -20.0;
const SKIN: f32 = 1e-3;
const MAX_SUBSTEP: f32 = 0.15;

fn character_box(pos: Vec3, s: &CharacterShape) -> Aabb {
    Aabb::new(
        Vec3::new(pos.x - s.radius, pos.y, pos.z - s.radius),
        Vec3::new(pos.x + s.radius, pos.y + s.height, pos.z + s.radius),
    )
}

fn blocked(world: &BoxWorld, pos: Vec3, s: &CharacterShape) -> bool {
    let b = character_box(pos, s);
    !world.boxes_in(&b).is_empty()
}

/// Сдвиг по одной горизонтальной оси с подъёмом на ступеньки. Возвращает новую позицию.
fn slide_axis(
    world: &BoxWorld,
    mut pos: Vec3,
    axis: usize,
    delta: f32,
    s: &CharacterShape,
) -> Vec3 {
    if delta == 0.0 {
        return pos;
    }
    let steps = (delta.abs() / MAX_SUBSTEP).ceil().max(1.0) as usize;
    let each = delta / steps as f32;
    for _ in 0..steps {
        let mut next = pos;
        next[axis] += each;
        if !blocked(world, next, s) {
            pos = next;
            continue;
        }
        // Препятствие: пробуем подняться на ступеньку (поверхность не выше step_height).
        let overlapping = world.boxes_in(&character_box(next, s));
        let top = overlapping
            .iter()
            .map(|(_, b)| b.max.y)
            .fold(f32::MIN, f32::max);
        let rise = top - pos.y;
        if rise > SKIN && rise <= s.step_height {
            let mut stepped = next;
            stepped.y = top + SKIN;
            if !blocked(world, stepped, s) {
                pos = stepped;
                continue;
            }
        }
        // Упор: прижимаемся вплотную к ближайшей грани и останавливаемся.
        let target = overlapping.iter().fold(None::<f32>, |acc, (_, b)| {
            let edge = if each > 0.0 {
                b.min[axis] - s.radius - SKIN
            } else {
                b.max[axis] + s.radius + SKIN
            };
            Some(match acc {
                None => edge,
                Some(a) => {
                    if each > 0.0 {
                        a.min(edge)
                    } else {
                        a.max(edge)
                    }
                }
            })
        });
        if let Some(t) = target {
            let candidate = if each > 0.0 {
                t.max(pos[axis])
            } else {
                t.min(pos[axis])
            };
            let mut slid = pos;
            slid[axis] = candidate;
            if !blocked(world, slid, s) {
                pos = slid;
            }
        }
        break;
    }
    pos
}

/// Один шаг движения: `wish` — желаемое горизонтальное смещение за шаг (Y игнорируется).
pub fn move_character(
    world: &BoxWorld,
    state: &mut CharacterState,
    shape: &CharacterShape,
    wish: Vec3,
    dt: f32,
) {
    let mut pos = state.pos;
    pos = slide_axis(world, pos, 0, wish.x, shape);
    pos = slide_axis(world, pos, 2, wish.z, shape);

    // Вертикаль.
    state.vel_y += GRAVITY * dt;
    let dy = state.vel_y * dt;
    let steps = (dy.abs() / MAX_SUBSTEP).ceil().max(1.0) as usize;
    let each = dy / steps as f32;
    let mut grounded = false;
    for _ in 0..steps {
        let mut next = pos;
        next.y += each;
        if !blocked(world, next, shape) {
            pos = next;
            continue;
        }
        let hit = world.boxes_in(&character_box(next, shape));
        if each < 0.0 {
            // Падение: встаём на самую высокую поверхность под ногами.
            let top = hit.iter().map(|(_, b)| b.max.y).fold(f32::MIN, f32::max);
            let landed = Vec3::new(pos.x, top + SKIN, pos.z);
            if !blocked(world, landed, shape) {
                pos = landed;
            }
            grounded = true;
        }
        state.vel_y = 0.0;
        break;
    }
    // Опора под ногами (малый зазор): стоит ли персонаж на поверхности.
    if !grounded {
        let probe = Vec3::new(pos.x, pos.y - 2.0 * SKIN, pos.z);
        grounded = blocked(world, probe, shape) && state.vel_y <= 0.0;
        if grounded {
            state.vel_y = 0.0;
        }
    }
    state.pos = pos;
    state.grounded = grounded;
}

/// Проверка: пересекает ли персонаж коробку (для триггеров и проверок размещения).
pub fn character_overlaps(state: &CharacterState, shape: &CharacterShape, b: &Aabb) -> bool {
    aabb_overlap(&state.aabb(shape), b)
}

/// Поверхность земли (рельеф): высота под точкой. Реализуется крейтом рельефа.
pub trait GroundProvider {
    /// Высота поверхности в точке (x, z) или `None`, если рельефа там нет.
    fn ground_height(&self, x: f32, z: f32) -> Option<f32>;
}

/// Шаг движения по коробкам мира **и** рельефу: персонаж стоит на земле, не заходит на
/// слишком крутые склоны (подъём больше `step_height` за шаг блокируется) и не проваливается.
pub fn move_character_on_ground(
    world: &BoxWorld,
    ground: &dyn GroundProvider,
    state: &mut CharacterState,
    shape: &CharacterShape,
    wish: Vec3,
    dt: f32,
) {
    let before = state.pos;
    let mut wish = wish;
    // Склон впереди: если земля поднимается круче допустимого, горизонтальное движение гасится.
    let probe = Vec3::new(before.x + wish.x, before.y, before.z + wish.z);
    if let (Some(h0), Some(h1)) = (
        ground.ground_height(before.x, before.z),
        ground.ground_height(probe.x, probe.z),
    ) && h1 - h0 > shape.step_height
    {
        wish = Vec3::ZERO;
    }
    move_character(world, state, shape, wish, dt);
    if let Some(h) = ground.ground_height(state.pos.x, state.pos.z)
        && state.pos.y <= h + 1e-3
    {
        state.pos.y = h;
        state.vel_y = 0.0;
        state.grounded = true;
    }
}
