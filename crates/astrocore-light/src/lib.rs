//! Свет и поле зрения Astrocore по образцу Robust: точечные источники (`PointLight`),
//! преграды (`Occluder`) и расчёт того, что освещено и что видно из точки.
//!
//! Крейт не рисует: он считает видимость по клеткам грида и силу света. Карту освещения
//! из этих расчётов собирает и показывает `astrocore-client`.

use astrocore_ecs::{GamePlugin, RegistryWorldExt};
use astrocore_map::{MapGrid, TileCoord};
use astrocore_math::{Color, Vec2};
use astrocore_net::NetRegistryWorldExt;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

fn default_radius() -> f32 {
    5.0
}

fn default_falloff() -> f32 {
    6.8
}

/// Точечный источник света. В данных — компонент `PointLight` (`PointLightComponent`
/// Robust @ 1afd692).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointLight {
    #[serde(default)]
    pub color: Color,
    /// Смещение источника от сущности.
    #[serde(default)]
    pub offset: Vec2,
    /// Яркость.
    #[serde(default = "one")]
    pub energy: f32,
    #[serde(default = "one")]
    pub softness: f32,
    /// Скорость затухания.
    #[serde(default = "default_falloff")]
    pub falloff: f32,
    /// Форма затухания: 0 — по расстоянию, 1 — по его квадрату.
    #[serde(default)]
    pub curve_factor: f32,
    #[serde(default = "yes")]
    pub cast_shadows: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Дальность в тайлах.
    #[serde(default = "default_radius")]
    pub radius: f32,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            offset: Vec2::ZERO,
            energy: 1.0,
            softness: 1.0,
            falloff: 6.8,
            curve_factor: 0.0,
            cast_shadows: true,
            enabled: true,
            radius: 5.0,
        }
    }
}

/// Высота источника над полом в формуле затухания (`LIGHTING_HEIGHT` шейдера Robust).
const LIGHTING_HEIGHT: f32 = 1.0;

impl PointLight {
    /// Доля яркости на расстоянии от источника, от 0 до 1 (`light_shared.swsl` Robust).
    pub fn attenuation(&self, distance: f32) -> f32 {
        if self.radius <= 0.0 {
            return 0.0;
        }
        let s = ((distance * distance + LIGHTING_HEIGHT).sqrt() / self.radius).clamp(0.0, 1.0);
        let s2 = s * s;
        let curve = s + (s2 - s) * self.curve_factor.clamp(0.0, 1.0);
        (((1.0 - s2) * (1.0 - s2)) / (1.0 + self.falloff * curve)).clamp(0.0, 1.0)
    }
}

/// Преграда для света и взгляда: стена, закрытая дверь. Закрывает свою клетку целиком,
/// пока привязана к ней. В данных — компонент `Occluder`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occluder {
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Default for Occluder {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Окно клеток грида с отметками «клетка закрыта преградой».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OccluderGrid {
    min_x: i32,
    min_y: i32,
    width: i32,
    height: i32,
    cells: Vec<bool>,
}

impl OccluderGrid {
    /// Пустое окно клеток от `(min_x, min_y)` размером `width` × `height`.
    pub fn new(min_x: i32, min_y: i32, width: i32, height: i32) -> Self {
        let (width, height) = (width.max(0), height.max(0));
        Self {
            min_x,
            min_y,
            width,
            height,
            cells: vec![false; (width * height) as usize],
        }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (dx, dy) = (x - self.min_x, y - self.min_y);
        (dx >= 0 && dy >= 0 && dx < self.width && dy < self.height)
            .then(|| (dy * self.width + dx) as usize)
    }

    pub fn set(&mut self, x: i32, y: i32, blocked: bool) {
        if let Some(index) = self.index(x, y) {
            self.cells[index] = blocked;
        }
    }

    /// Клетка закрыта. Клетки вне окна открыты.
    pub fn blocked(&self, x: i32, y: i32) -> bool {
        self.index(x, y).is_some_and(|index| self.cells[index])
    }

    /// Отметки клеток в квадрате вокруг клетки: по ним видно, изменились ли преграды
    /// рядом с источником света.
    pub fn window(&self, x: i32, y: i32, radius: i32) -> Vec<bool> {
        (y - radius..=y + radius)
            .flat_map(|cy| (x - radius..=x + radius).map(move |cx| (cx, cy)))
            .map(|(cx, cy)| self.blocked(cx, cy))
            .collect()
    }

    /// Точка `to` видна из точки `from` (координаты грида в тайлах): между ними нет
    /// закрытых клеток. Клетки самих точек не мешают: стена, до которой дошёл взгляд,
    /// видна, а наблюдатель в дверном проёме видит из него.
    pub fn visible(&self, from: Vec2, to: Vec2) -> bool {
        let (mut x, mut y) = (from.x.floor() as i32, from.y.floor() as i32);
        let (end_x, end_y) = (to.x.floor() as i32, to.y.floor() as i32);
        let delta = to - from;
        let (step_x, step_y) = (delta.x.signum() as i32, delta.y.signum() as i32);
        // Доля пути до следующей границы клетки по каждой оси и шаг этой доли.
        let boundary = |position: f32, cell: i32, step: i32, delta: f32| {
            if step == 0 {
                f32::INFINITY
            } else {
                let edge = if step > 0 { cell + 1 } else { cell } as f32;
                (edge - position) / delta
            }
        };
        let mut next_x = boundary(from.x, x, step_x, delta.x);
        let mut next_y = boundary(from.y, y, step_y, delta.y);
        let (pace_x, pace_y) = (
            if step_x == 0 {
                f32::INFINITY
            } else {
                (1.0 / delta.x).abs()
            },
            if step_y == 0 {
                f32::INFINITY
            } else {
                (1.0 / delta.y).abs()
            },
        );
        // Путь не длиннее числа клеток между точками по обеим осям.
        let limit = (end_x - x).abs() + (end_y - y).abs();
        for _ in 0..limit {
            if next_x < next_y {
                x += step_x;
                next_x += pace_x;
            } else {
                y += step_y;
                next_y += pace_y;
            }
            if x == end_x && y == end_y {
                return true;
            }
            if self.blocked(x, y) {
                return false;
            }
        }
        true
    }
}

/// Собирает преграды грида на этаже `z` в окне клеток.
pub fn collect_occluders(
    world: &World,
    grid: Entity,
    z: i16,
    min: (i32, i32),
    size: (i32, i32),
) -> OccluderGrid {
    let mut occluders = OccluderGrid::new(min.0, min.1, size.0, size.1);
    let Some(map_grid) = world.get::<MapGrid>(grid) else {
        return occluders;
    };
    for y in min.1..min.1 + occluders.height {
        for x in min.0..min.0 + occluders.width {
            let blocked = map_grid
                .anchored_at(TileCoord::new(x, y, z))
                .iter()
                .any(|entity| world.get::<Occluder>(*entity).is_some_and(|o| o.enabled));
            if blocked {
                occluders.set(x, y, true);
            }
        }
    }
    occluders
}

/// Свет: одинаково подключается на сервере и клиенте.
pub struct LightPlugin;

impl GamePlugin for LightPlugin {
    fn build(&self, world: &mut World) {
        world.register_data_component::<PointLight>();
        world.register_data_component::<Occluder>();
        world.register_net_serde::<PointLight>();
        world.register_net_serde::<Occluder>();
    }
}

#[cfg(test)]
mod tests {
    use astrocore_data::{from_node, parse_yaml};

    use super::*;

    #[test]
    fn light_reads_ss14_data_and_fades_with_distance() {
        let light: PointLight = from_node(
            &parse_yaml(
                "color: \"#FFE4CE\"\nenergy: 0.8\nradius: 10\nsoftness: 1\ncastShadows: false\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (light.energy, light.radius, light.cast_shadows),
            (0.8, 10.0, false)
        );
        assert_eq!(light.falloff, 6.8);
        assert!(light.enabled);
        let near = light.attenuation(0.0);
        let middle = light.attenuation(5.0);
        assert!(near > middle && middle > 0.0, "{near} {middle}");
        assert_eq!(light.attenuation(10.0), 0.0);
        assert_eq!(light.attenuation(30.0), 0.0);
    }

    /// Стена по клеткам x = 3, y = 0..=4 с проёмом в y = 2.
    fn wall_with_gap() -> OccluderGrid {
        let mut grid = OccluderGrid::new(-2, -2, 12, 12);
        for y in [0, 1, 3, 4] {
            grid.set(3, y, true);
        }
        grid
    }

    #[test]
    fn walls_block_sight_and_gaps_let_it_through() {
        let grid = wall_with_gap();
        let eye = Vec2::new(1.5, 2.5);
        // Сквозь проём видно, сквозь стену — нет.
        assert!(grid.visible(eye, Vec2::new(6.5, 2.5)));
        assert!(!grid.visible(eye, Vec2::new(6.5, 0.5)));
        assert!(!grid.visible(Vec2::new(1.5, 0.5), Vec2::new(6.5, 0.5)));
        // Сама стена, до которой дошёл взгляд, видна; клетка за ней — нет.
        assert!(grid.visible(eye, Vec2::new(3.5, 0.5)));
        assert!(!grid.visible(Vec2::new(1.5, 0.5), Vec2::new(4.5, 0.5)));
        // Взгляд симметричен и работает во все стороны.
        assert!(grid.visible(Vec2::new(6.5, 2.5), eye));
        assert!(!grid.visible(Vec2::new(6.5, 0.5), eye));
        assert!(grid.visible(eye, Vec2::new(1.5, 9.5)));
        assert!(grid.visible(eye, eye));
        // Точки вне окна клеток открыты.
        assert!(grid.visible(Vec2::new(-10.5, -10.5), Vec2::new(-5.5, -7.5)));
    }

    #[test]
    fn window_shows_changes_near_a_light() {
        let mut grid = wall_with_gap();
        let before = grid.window(4, 2, 2);
        assert_eq!(before.len(), 25);
        grid.set(3, 2, true);
        assert_ne!(grid.window(4, 2, 2), before);
        // Преграда далеко от источника его окно не меняет.
        let near_light = grid.window(8, 8, 1);
        grid.set(0, 0, true);
        assert_eq!(grid.window(8, 8, 1), near_light);
    }
}
