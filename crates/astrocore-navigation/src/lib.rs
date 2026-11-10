//! Навигационные запросы поверх канонического мира (раздел 14 дизайн-документа).
//!
//! Текущая версия — **запрос пути по сетке на одном этаже**: проходимость
//! читается непосредственно из канонического мира, поэтому после разрушения
//! стены результат сразу корректен. Кэш навигационных тайлов с инкрементальной
//! перестройкой по [`astrocore_world::DirtyRegion`] и связи между этажами
//! (лестницы, лифты) — следующий этап, см. `docs/PROGRESS.md`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use astrocore_core::ObjectId;
use astrocore_math::{GridCell, GridEdge};
use astrocore_world::{FloorId, OpeningKind, StructureKind, World};

/// Верхняя граница числа раскрытых ячеек за один запрос (защита сервера).
pub const DEFAULT_NODE_LIMIT: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("этаж {0:?} не определён")]
    UnknownFloor(FloorId),
    #[error("путь не найден")]
    NoPath,
    #[error("превышен лимит поиска ({0} ячеек)")]
    LimitExceeded(usize),
}

/// Параметры запроса.
#[derive(Debug, Clone)]
pub struct PathOptions {
    /// Двери, которые сейчас закрыты и непроходимы. Остальные проёмы-двери проходимы.
    pub closed_doors: HashSet<ObjectId>,
    pub node_limit: usize,
}

impl Default for PathOptions {
    fn default() -> Self {
        Self {
            closed_doors: HashSet::new(),
            node_limit: DEFAULT_NODE_LIMIT,
        }
    }
}

/// Можно ли перейти между двумя соседними ячейками.
pub fn can_step(
    world: &World,
    floor: FloorId,
    a: GridCell,
    b: GridCell,
    opts: &PathOptions,
) -> bool {
    let Some(edge) = GridEdge::between(a, b) else {
        return false;
    };
    match world.edge_element(floor, edge) {
        None => true,
        Some(e) => match e.kind {
            StructureKind::Opening(OpeningKind::Door) => !opts.closed_doors.contains(&e.id),
            // Стены и окна непроходимы.
            _ => false,
        },
    }
}

/// Кратчайший путь по ячейкам (A*, 4-связность, манхэттенская эвристика).
/// Возвращает ячейки от `start` до `goal` включительно.
pub fn find_path(
    world: &World,
    floor: FloorId,
    start: GridCell,
    goal: GridCell,
    opts: &PathOptions,
) -> Result<Vec<GridCell>, PathError> {
    if world.floor(floor).is_none() {
        return Err(PathError::UnknownFloor(floor));
    }
    if start == goal {
        return Ok(vec![start]);
    }
    let h = |c: GridCell| ((c.x - goal.x).abs() + (c.z - goal.z).abs()) as u32;

    // Окно поиска: постройки этажа + старт + цель + кольцо в одну ячейку. За пределами
    // построек всё свободно, кольцо сохраняет связность, а окно позволяет честно
    // вернуть `NoPath`, а не искать бесконечно по открытой плоскости.
    let (mut lo, mut hi) = world.floor_cell_bounds(floor).unwrap_or((start, start));
    for c in [start, goal] {
        lo = GridCell::new(lo.x.min(c.x), lo.z.min(c.z));
        hi = GridCell::new(hi.x.max(c.x), hi.z.max(c.z));
    }
    let (lo, hi) = (lo.offset(-1, -1), hi.offset(1, 1));
    let in_window = |c: GridCell| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z;

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<GridCell, u32> = HashMap::from([(start, 0)]);
    let mut came_from: HashMap<GridCell, GridCell> = HashMap::new();
    let mut closed: HashSet<GridCell> = HashSet::new();
    open.push(Reverse((h(start), 0u32, start)));

    while let Some(Reverse((_, g, cur))) = open.pop() {
        if cur == goal {
            let mut path = vec![cur];
            let mut at = cur;
            while let Some(&prev) = came_from.get(&at) {
                path.push(prev);
                at = prev;
            }
            path.reverse();
            return Ok(path);
        }
        if !closed.insert(cur) {
            continue;
        }
        if closed.len() > opts.node_limit {
            return Err(PathError::LimitExceeded(opts.node_limit));
        }
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let next = cur.offset(dx, dz);
            if !in_window(next)
                || closed.contains(&next)
                || !can_step(world, floor, cur, next, opts)
            {
                continue;
            }
            let ng = g + 1;
            if g_score.get(&next).is_none_or(|&old| ng < old) {
                g_score.insert(next, ng);
                came_from.insert(next, cur);
                open.push(Reverse((ng + h(next), ng, next)));
            }
        }
    }
    Err(PathError::NoPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrocore_core::PrototypeId;
    use astrocore_world::{FloorDef, Placement, StructureElement, WorldCommand, WorldConfig};

    fn world() -> World {
        let mut w = World::new("t", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 0.0,
            height: 3.0,
        }))
        .unwrap();
        w
    }

    fn put(w: &mut World, id: u128, kind: StructureKind, edge: GridEdge) {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement: Placement::Edge(edge),
            floor: FloorId(0),
            material: PrototypeId::new("t:m").unwrap(),
            thickness: 0.2,
        }))
        .unwrap();
    }

    /// Комната 3×3; в северной стене на ребре (1,0) — вход (id=900).
    fn room_with_door(w: &mut World, door: bool) {
        let mut n = 0;
        for i in 0..3 {
            for e in [
                GridEdge::along_x(i, 0),
                GridEdge::along_x(i, 3),
                GridEdge::along_z(0, i),
                GridEdge::along_z(3, i),
            ] {
                if door && e == GridEdge::along_x(1, 0) {
                    put(w, 900, StructureKind::Opening(OpeningKind::Door), e);
                } else {
                    put(w, n, StructureKind::Wall, e);
                }
                n += 1;
            }
        }
    }

    const OUTSIDE: GridCell = GridCell::new(1, -2);
    const INSIDE: GridCell = GridCell::new(1, 1);

    #[test]
    fn path_goes_through_open_door() {
        let mut w = world();
        room_with_door(&mut w, true);
        let p = find_path(&w, FloorId(0), OUTSIDE, INSIDE, &PathOptions::default()).unwrap();
        assert_eq!(p.first(), Some(&OUTSIDE));
        assert_eq!(p.last(), Some(&INSIDE));
        assert!(p.contains(&GridCell::new(1, -1)) && p.contains(&GridCell::new(1, 0)));
        assert_eq!(p.len(), 4, "кратчайший путь прямо через дверь");
    }

    #[test]
    fn closed_door_blocks_path() {
        let mut w = world();
        room_with_door(&mut w, true);
        let opts = PathOptions {
            closed_doors: HashSet::from([ObjectId::from_u128(900)]),
            ..Default::default()
        };
        assert_eq!(
            find_path(&w, FloorId(0), OUTSIDE, INSIDE, &opts),
            Err(PathError::NoPath)
        );
    }

    #[test]
    fn destroying_a_wall_opens_a_new_path() {
        let mut w = world();
        room_with_door(&mut w, false);
        let opts = PathOptions {
            node_limit: 5_000,
            ..Default::default()
        };
        assert_eq!(
            find_path(&w, FloorId(0), OUTSIDE, INSIDE, &opts),
            Err(PathError::NoPath)
        );
        // Разрушили южную стену (ребро (1,3)).
        let id = w
            .edge_element(FloorId(0), GridEdge::along_x(1, 3))
            .unwrap()
            .id;
        w.apply(WorldCommand::RemoveElement { id }).unwrap();
        let p = find_path(&w, FloorId(0), OUTSIDE, INSIDE, &opts).unwrap();
        assert_eq!(p.last(), Some(&INSIDE));
        assert!(p.contains(&GridCell::new(1, 3)), "путь идёт через пролом");
    }

    #[test]
    fn windows_and_walls_block_but_trivial_and_unknown_cases_work() {
        let mut w = world();
        room_with_door(&mut w, false);
        let win = w
            .edge_element(FloorId(0), GridEdge::along_x(0, 0))
            .unwrap()
            .id;
        w.apply(WorldCommand::RemoveElement { id: win }).unwrap();
        put(
            &mut w,
            800,
            StructureKind::Opening(OpeningKind::Window),
            GridEdge::along_x(0, 0),
        );
        let opts = PathOptions {
            node_limit: 5_000,
            ..Default::default()
        };
        assert_eq!(
            find_path(
                &w,
                FloorId(0),
                GridCell::new(0, -1),
                GridCell::new(0, 0),
                &opts
            ),
            Err(PathError::NoPath)
        );
        assert_eq!(
            find_path(&w, FloorId(0), INSIDE, INSIDE, &opts),
            Ok(vec![INSIDE])
        );
        assert_eq!(
            find_path(&w, FloorId(3), INSIDE, INSIDE, &opts),
            Err(PathError::UnknownFloor(FloorId(3)))
        );
    }

    #[test]
    fn search_limit_protects_server() {
        let w = world();
        let opts = PathOptions {
            node_limit: 50,
            ..Default::default()
        };
        let far = GridCell::new(10_000, 10_000);
        assert_eq!(
            find_path(&w, FloorId(0), GridCell::new(0, 0), far, &opts),
            Err(PathError::LimitExceeded(50))
        );
    }

    #[test]
    fn open_field_path_is_shortest_manhattan() {
        let w = world();
        let p = find_path(
            &w,
            FloorId(0),
            GridCell::new(-3, -2),
            GridCell::new(2, 4),
            &PathOptions::default(),
        )
        .unwrap();
        assert_eq!(p.len(), 1 + 5 + 6);
    }
}
