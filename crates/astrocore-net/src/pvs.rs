//! PVS: что видит сессия. Образец — `Robust.Server/GameStates/PvsSystem*.cs` @ 1afd692.
//!
//! Сущности, стоящие прямо на гриде или в космосе карты, лежат в клетках индекса размером
//! [`PVS_CHUNK_SIZE`] тайлов по этажам. Клиент получает сущности клеток вокруг своего глаза
//! на его этаже, всех их потомков (содержимое контейнеров), карту и гриды карты.

use std::collections::HashMap;

use astrocore_ecs::EventWorldExt;
use astrocore_map::{Map, MapGrid, MoveEvent, Transform, TransformWorldExt};
use astrocore_math::Vec2;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::Children;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

/// Сторона клетки индекса в тайлах.
pub const PVS_CHUNK_SIZE: f32 = 8.0;

/// Клетка индекса: корень (грид или карта), координаты клетки и этаж.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PvsCell {
    pub root: Entity,
    pub x: i32,
    pub y: i32,
    pub z: i16,
}

impl PvsCell {
    fn containing(root: Entity, position: Vec2, z: i16) -> Self {
        Self {
            root,
            x: (position.x / PVS_CHUNK_SIZE).floor() as i32,
            y: (position.y / PVS_CHUNK_SIZE).floor() as i32,
            z,
        }
    }
}

/// Клетка, в которой сейчас числится сущность.
#[derive(Component, Debug, Clone, Copy)]
struct PvsPlace(PvsCell);

/// Индекс сущностей по клеткам. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct PvsIndex {
    cells: HashMap<PvsCell, Vec<Entity>>,
}

impl PvsIndex {
    fn remove(&mut self, cell: PvsCell, entity: Entity) {
        if let Some(entities) = self.cells.get_mut(&cell) {
            entities.retain(|e| *e != entity);
            if entities.is_empty() {
                self.cells.remove(&cell);
            }
        }
    }

    /// Сущности клетки.
    pub fn cell(&self, cell: PvsCell) -> &[Entity] {
        self.cells.get(&cell).map_or(&[], Vec::as_slice)
    }

    /// Число занятых клеток.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// Сущности, которые видны независимо от расстояния (`PvsOverrideSystem` Robust).
#[derive(Resource, Debug, Default)]
pub struct PvsOverrides {
    /// Видны всем сессиям.
    pub global: Vec<Entity>,
    /// Видны одной сессии: сессия → сущности.
    pub per_session: HashMap<Entity, Vec<Entity>>,
}

/// Переносит сущность в клетку, соответствующую её положению.
fn update_place(world: &mut World, entity: Entity) {
    let cell = world.get::<Transform>(entity).and_then(|transform| {
        // Гриды и карты видны всегда и в клетках не числятся.
        if world.get::<MapGrid>(entity).is_some() || world.get::<Map>(entity).is_some() {
            return None;
        }
        let parent = world.parent(entity)?;
        let top_level =
            world.get::<MapGrid>(parent).is_some() || world.get::<Map>(parent).is_some();
        top_level
            .then(|| PvsCell::containing(parent, transform.local_position(), transform.level()))
    });
    let previous = world.get::<PvsPlace>(entity).map(|place| place.0);
    if previous == cell {
        return;
    }
    let mut index = world.get_resource_or_insert_with(PvsIndex::default);
    if let Some(previous) = previous {
        index.remove(previous, entity);
    }
    if let Some(cell) = cell {
        index.cells.entry(cell).or_default().push(entity);
    }
    match cell {
        Some(cell) => {
            world.entity_mut(entity).insert(PvsPlace(cell));
        }
        None => {
            if let Ok(mut target) = world.get_entity_mut(entity) {
                target.remove::<PvsPlace>();
            }
        }
    }
}

/// Откуда и как далеко смотрит сессия.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// Сущность карты.
    pub map: Entity,
    /// Точка в мировых координатах карты.
    pub position: Vec2,
    /// Этаж карты.
    pub level: i16,
    /// Полуширина квадрата видимости в тайлах.
    pub range: f32,
}

/// Видимые сущности: группами «сущность верхнего уровня и все её потомки», родитель всегда
/// раньше потомка.
#[derive(Debug, Default)]
pub struct Visible {
    pub entities: Vec<Entity>,
    /// Начала групп в `entities`.
    pub groups: Vec<u32>,
}

impl Visible {
    fn clear(&mut self) {
        self.entities.clear();
        self.groups.clear();
    }

    fn push_group(&mut self, world: &World, root: Entity, descend: bool) {
        if world.get::<Transform>(root).is_none() {
            return;
        }
        self.groups.push(self.entities.len() as u32);
        let start = self.entities.len();
        self.entities.push(root);
        if !descend {
            return;
        }
        // Обход в ширину: родитель попадает в список раньше своих потомков.
        let mut next = start;
        while next < self.entities.len() {
            if let Some(children) = world.get::<Children>(self.entities[next]) {
                self.entities.extend(children.iter().copied());
            }
            next += 1;
        }
    }

    /// Группы: сущность верхнего уровня и её потомки.
    pub fn iter_groups(&self) -> impl Iterator<Item = &[Entity]> {
        self.groups.iter().enumerate().map(|(index, &start)| {
            let end = self
                .groups
                .get(index + 1)
                .map_or(self.entities.len(), |&next| next as usize);
            &self.entities[start as usize..end]
        })
    }
}

fn push_cells(
    world: &World,
    index: &PvsIndex,
    root: Entity,
    center: Vec2,
    z: i16,
    range: f32,
    out: &mut Visible,
) {
    let min = PvsCell::containing(root, center - Vec2::splat(range), z);
    let max = PvsCell::containing(root, center + Vec2::splat(range), z);
    let middle = PvsCell::containing(root, center, z);
    // Ближние клетки идут первыми: при ограничении числа новых сущностей за тик клиент
    // сначала получает то, что рядом.
    let mut cells: Vec<(i32, i32)> = (min.x..=max.x)
        .flat_map(|x| (min.y..=max.y).map(move |y| (x, y)))
        .collect();
    cells.sort_by_key(|(x, y)| (x - middle.x).abs().max((y - middle.y).abs()));
    for (x, y) in cells {
        for &entity in index.cell(PvsCell { root, x, y, z }) {
            out.push_group(world, entity, true);
        }
    }
}

/// Собирает сущности, видимые из точки.
pub fn collect_visible(world: &World, index: &PvsIndex, view: &View, out: &mut Visible) {
    out.clear();
    let Some(map) = world.get::<Map>(view.map) else {
        return;
    };
    out.push_group(world, view.map, false);
    for &grid in map.grids() {
        out.push_group(world, grid, false);
    }
    push_cells(
        world,
        index,
        view.map,
        view.position,
        view.level,
        view.range,
        out,
    );
    for &grid in map.grids() {
        let Some(placement) = world.world_transform(grid) else {
            continue;
        };
        // Квадрат видимости в координатах повёрнутого грида покрывается квадратом побольше.
        let range = if placement.rotation.0 == 0.0 {
            view.range
        } else {
            view.range * std::f32::consts::SQRT_2
        };
        push_cells(
            world,
            index,
            grid,
            placement.to_local(view.position),
            view.level - placement.level,
            range,
            out,
        );
    }
}

/// Подписки, поддерживающие индекс.
pub(crate) fn register(world: &mut World) {
    use astrocore_ecs::EntityTerminating;

    world.init_resource::<PvsIndex>();
    world.init_resource::<PvsOverrides>();
    world.subscribe::<Transform, MoveEvent>(|world, entity, _| update_place(world, entity));
    world.subscribe::<Transform, EntityTerminating>(|world, entity, _| {
        if let Some(place) = world.get::<PvsPlace>(entity).map(|place| place.0) {
            world.resource_mut::<PvsIndex>().remove(place, entity);
        }
    });
}
