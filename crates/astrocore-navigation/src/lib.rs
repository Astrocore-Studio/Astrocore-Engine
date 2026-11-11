//! Навигационные запросы поверх канонического мира (раздел 14 дизайн-документа).
//!
//! Текущая версия — **запрос пути по сетке на одном этаже**: проходимость
//! читается непосредственно из канонического мира, поэтому после разрушения
//! стены результат сразу корректен. Кэш навигационных тайлов с инкрементальной
//! перестройкой по [`astrocore_world::DirtyRegion`] и связи между этажами
//! (лестницы, лифты) — следующий этап, см. `docs/PROGRESS.md`.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};

use astrocore_core::ObjectId;
use astrocore_math::{ChunkCoord, GridCell, GridEdge};
use astrocore_world::{DirtyFlags, DirtyRegion, FloorId, OpeningKind, StructureKind, World};

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

/// Узел многоэтажного графа: этаж и ячейка.
pub type Node = (FloorId, GridCell);

/// Источник данных о проходимости: канонический мир напрямую или кэш тайлов [`NavGrid`].
pub trait NavSource {
    fn has_floor(&self, floor: FloorId) -> bool;
    fn can_step(&self, floor: FloorId, a: GridCell, b: GridCell, opts: &PathOptions) -> bool;
    /// Вертикальные связи: (вход, выход, стоимость). Двусторонние.
    fn links(&self) -> Vec<(Node, Node, u32)>;
    /// Ячейки, по которым строится окно поиска (границы построек).
    fn extent_points(&self) -> Vec<GridCell>;
}

impl NavSource for World {
    fn has_floor(&self, floor: FloorId) -> bool {
        self.floor(floor).is_some()
    }

    fn can_step(&self, floor: FloorId, a: GridCell, b: GridCell, opts: &PathOptions) -> bool {
        can_step(self, floor, a, b, opts)
    }

    fn links(&self) -> Vec<(Node, Node, u32)> {
        self.vertical_links()
            .into_iter()
            .map(|l| {
                let len = self.element(l.element).map_or(1, |e| match e.placement {
                    astrocore_world::Placement::Run { length, .. } => u32::from(length) + 1,
                    _ => 1,
                });
                (l.lower, l.upper, len)
            })
            .collect()
    }

    fn extent_points(&self) -> Vec<GridCell> {
        let mut pts = Vec::new();
        for f in self.floors() {
            if let Some((lo, hi)) = self.floor_cell_bounds(f.id) {
                pts.push(lo);
                pts.push(hi);
            }
        }
        pts
    }
}

/// Кэш навигационных тайлов (чанк × этаж). Тайл хранит рёбра, которые блокируют движение,
/// и рёбра дверей. Обновляется **инкрементально** по dirty-регионам с флагом `NAVIGATION`:
/// после разрушения стены пересобирается только затронутый тайл, а не весь город.
#[derive(Debug, Default, Clone)]
pub struct NavGrid {
    chunk_size_m: f32,
    grid_step_m: f32,
    floors: BTreeSet<FloorId>,
    tiles: BTreeMap<(ChunkCoord, FloorId), Tile>,
    links: Vec<(Node, Node, u32)>,
    rebuilt_tiles: usize,
}

#[derive(Debug, Default, Clone)]
struct Tile {
    /// Рёбра, через которые нельзя пройти никогда (стены, окна).
    solid: BTreeSet<GridEdge>,
    /// Рёбра дверей: проходимость зависит от состояния двери.
    doors: BTreeMap<GridEdge, ObjectId>,
}

impl NavGrid {
    pub fn build(world: &World) -> Self {
        let cfg = world.config();
        let mut g = Self {
            chunk_size_m: cfg.chunk_size_m,
            grid_step_m: cfg.grid_step_m,
            ..Self::default()
        };
        g.floors = world.floors().map(|f| f.id).collect();
        for floor in g.floors.clone() {
            for chunk in world.populated_chunks(floor) {
                g.rebuild_tile(world, chunk, floor);
            }
        }
        g.links = world.links();
        g.rebuilt_tiles = 0;
        g
    }

    /// Сколько тайлов было пересобрано с последнего вызова [`NavGrid::take_rebuilt`].
    pub fn take_rebuilt(&mut self) -> usize {
        std::mem::take(&mut self.rebuilt_tiles)
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Применить изменения мира (результат `World::take_dirty`).
    pub fn apply_dirty(&mut self, world: &World, regions: &[DirtyRegion]) {
        self.floors = world.floors().map(|f| f.id).collect();
        for r in regions {
            if r.flags.contains(DirtyFlags::NAVIGATION) {
                self.rebuild_tile(world, r.chunk, r.floor);
            }
        }
        self.links = world.links();
    }

    /// Тайл, которому принадлежит ребро: чанк «большей» из двух смежных ячеек.
    fn tile_of(&self, edge: GridEdge) -> ChunkCoord {
        edge.adjacent_cells()[1].chunk(self.grid_step_m, self.chunk_size_m)
    }

    fn rebuild_tile(&mut self, world: &World, chunk: ChunkCoord, floor: FloorId) {
        self.rebuilt_tiles += 1;
        let mut tile = Tile::default();
        for e in world.elements_in_chunk(chunk, floor) {
            let astrocore_world::Placement::Edge(edge) = e.placement else {
                continue;
            };
            if self.tile_of(edge) != chunk {
                continue;
            }
            match e.kind {
                StructureKind::Wall | StructureKind::Opening(OpeningKind::Window) => {
                    tile.solid.insert(edge);
                }
                StructureKind::Opening(OpeningKind::Door) => {
                    tile.doors.insert(edge, e.id);
                }
                _ => {}
            }
        }
        if tile.solid.is_empty() && tile.doors.is_empty() {
            self.tiles.remove(&(chunk, floor));
        } else {
            self.tiles.insert((chunk, floor), tile);
        }
    }
}

impl NavSource for NavGrid {
    fn has_floor(&self, floor: FloorId) -> bool {
        self.floors.contains(&floor)
    }

    fn can_step(&self, floor: FloorId, a: GridCell, b: GridCell, opts: &PathOptions) -> bool {
        let Some(edge) = GridEdge::between(a, b) else {
            return false;
        };
        let Some(tile) = self.tiles.get(&(self.tile_of(edge), floor)) else {
            return true;
        };
        if tile.solid.contains(&edge) {
            return false;
        }
        tile.doors
            .get(&edge)
            .is_none_or(|id| !opts.closed_doors.contains(id))
    }

    fn links(&self) -> Vec<(Node, Node, u32)> {
        self.links.clone()
    }

    fn extent_points(&self) -> Vec<GridCell> {
        let mut pts = Vec::new();
        for tile in self.tiles.values() {
            for e in tile.solid.iter().chain(tile.doors.keys()) {
                pts.extend(e.adjacent_cells());
            }
        }
        pts
    }
}

/// Кратчайший путь между этажами (A*): шаги по сетке этажей + переходы по лестницам
/// и пандусам (двусторонние, стоимость — длина марша).
///
/// Ячейки марша на нижнем этаже обычные проходимые; подъём моделируется переходом
/// из входа марша на выход верхнего этажа.
pub fn find_path_3d(
    world: &impl NavSource,
    start: Node,
    goal: Node,
    opts: &PathOptions,
) -> Result<Vec<Node>, PathError> {
    for (floor, _) in [start, goal] {
        if !world.has_floor(floor) {
            return Err(PathError::UnknownFloor(floor));
        }
    }
    if start == goal {
        return Ok(vec![start]);
    }
    let mut jumps: HashMap<Node, Vec<(Node, u32)>> = HashMap::new();
    for (lower, upper, len) in world.links() {
        jumps.entry(lower).or_default().push((upper, len));
        jumps.entry(upper).or_default().push((lower, len));
    }

    // Общее окно поиска: постройки всех этажей + старт, цель, концы связей + кольцо.
    let mut points: Vec<GridCell> = vec![start.1, goal.1];
    points.extend(world.extent_points());
    points.extend(jumps.keys().map(|n| n.1));
    let lo = GridCell::new(
        points.iter().map(|c| c.x).min().unwrap_or(0) - 1,
        points.iter().map(|c| c.z).min().unwrap_or(0) - 1,
    );
    let hi = GridCell::new(
        points.iter().map(|c| c.x).max().unwrap_or(0) + 1,
        points.iter().map(|c| c.z).max().unwrap_or(0) + 1,
    );
    let in_window = |c: GridCell| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z;
    let h = |n: Node| ((n.1.x - goal.1.x).abs() + (n.1.z - goal.1.z).abs()) as u32;

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<Node, u32> = HashMap::from([(start, 0)]);
    let mut came_from: HashMap<Node, Node> = HashMap::new();
    let mut closed: HashSet<Node> = HashSet::new();
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
        let (floor, cell) = cur;
        let mut next: Vec<(Node, u32)> = Vec::new();
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = cell.offset(dx, dz);
            if in_window(n) && world.can_step(floor, cell, n, opts) {
                next.push(((floor, n), 1));
            }
        }
        if let Some(js) = jumps.get(&cur) {
            next.extend(js.iter().copied());
        }
        for (n, cost) in next {
            if closed.contains(&n) {
                continue;
            }
            let ng = g + cost;
            if g_score.get(&n).is_none_or(|&old| ng < old) {
                g_score.insert(n, ng);
                came_from.insert(n, cur);
                open.push(Reverse((ng + h(n), ng, n)));
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

    mod multi_floor {
        use super::*;
        use astrocore_math::Facing;

        /// Два этажа; на нижнем — лестница из (5,5) на юг длиной 3 (выход на втором этаже в (5,8)).
        fn two_floors() -> World {
            let mut w = world();
            w.apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(1),
                name: "up".into(),
                base_y: 3.0,
                height: 3.0,
            }))
            .unwrap();
            w.apply(WorldCommand::PlaceElement(StructureElement {
                id: ObjectId::from_u128(77),
                kind: StructureKind::Stair { to: FloorId(1) },
                placement: Placement::Run {
                    start: GridCell::new(5, 5),
                    dir: Facing::South,
                    length: 3,
                },
                floor: FloorId(0),
                material: PrototypeId::new("t:m").unwrap(),
                thickness: 0.2,
            }))
            .unwrap();
            w
        }

        #[test]
        fn path_climbs_stairs_between_floors() {
            let w = two_floors();
            let start = (FloorId(0), GridCell::new(5, 3));
            let goal = (FloorId(1), GridCell::new(7, 8));
            let path = find_path_3d(&w, start, goal, &PathOptions::default()).unwrap();
            assert_eq!(path.first(), Some(&start));
            assert_eq!(path.last(), Some(&goal));
            let jump = path
                .windows(2)
                .position(|p| p[0].0 != p[1].0)
                .expect("есть переход между этажами");
            assert_eq!(
                path[jump],
                (FloorId(0), GridCell::new(5, 5)),
                "вход на марш"
            );
            assert_eq!(
                path[jump + 1],
                (FloorId(1), GridCell::new(5, 8)),
                "выход наверху"
            );
        }

        #[test]
        fn links_work_in_both_directions_and_without_stairs_there_is_no_path() {
            let w = two_floors();
            let down = find_path_3d(
                &w,
                (FloorId(1), GridCell::new(5, 8)),
                (FloorId(0), GridCell::new(5, 4)),
                &PathOptions::default(),
            )
            .unwrap();
            assert_eq!(down.last(), Some(&(FloorId(0), GridCell::new(5, 4))));

            let mut no_stairs = world();
            no_stairs
                .apply(WorldCommand::DefineFloor(FloorDef {
                    id: FloorId(1),
                    name: "up".into(),
                    base_y: 3.0,
                    height: 3.0,
                }))
                .unwrap();
            assert_eq!(
                find_path_3d(
                    &no_stairs,
                    (FloorId(0), GridCell::new(0, 0)),
                    (FloorId(1), GridCell::new(0, 0)),
                    &PathOptions::default()
                ),
                Err(PathError::NoPath)
            );
            assert_eq!(
                find_path_3d(
                    &no_stairs,
                    (FloorId(9), GridCell::new(0, 0)),
                    (FloorId(1), GridCell::new(0, 0)),
                    &PathOptions::default()
                ),
                Err(PathError::UnknownFloor(FloorId(9)))
            );
        }
    }

    mod tiles {
        use super::*;

        struct Lcg(u64);
        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 33
            }
            fn range(&mut self, n: u64) -> i32 {
                (self.next() % n) as i32
            }
        }

        fn random_world(seed: u64) -> (World, Lcg) {
            let mut rng = Lcg(seed);
            let mut w = world();
            for i in 0..140u128 {
                let (x, z) = (rng.range(70) - 5, rng.range(24) - 4); // пересекает границы чанков 0/1/2 по X
                let edge = if rng.range(2) == 0 {
                    GridEdge::along_x(x, z)
                } else {
                    GridEdge::along_z(x, z)
                };
                let kind = match rng.range(10) {
                    0 => StructureKind::Opening(OpeningKind::Door),
                    1 => StructureKind::Opening(OpeningKind::Window),
                    _ => StructureKind::Wall,
                };
                let _ = w.apply(WorldCommand::PlaceElement(StructureElement {
                    id: ObjectId::from_u128(1 + i),
                    kind,
                    placement: Placement::Edge(edge),
                    floor: FloorId(0),
                    material: PrototypeId::new("t:m").unwrap(),
                    thickness: 0.2,
                }));
            }
            (w, rng)
        }

        fn same_result(w: &World, grid: &NavGrid, rng: &mut Lcg, opts: &PathOptions) {
            for _ in 0..25 {
                let a = (
                    FloorId(0),
                    GridCell::new(rng.range(70) - 5, rng.range(24) - 4),
                );
                let b = (
                    FloorId(0),
                    GridCell::new(rng.range(70) - 5, rng.range(24) - 4),
                );
                let direct = find_path_3d(w, a, b, opts).map(|p| p.len());
                let cached = find_path_3d(grid, a, b, opts).map(|p| p.len());
                assert_eq!(direct, cached, "{a:?} → {b:?}");
            }
        }

        #[test]
        fn tile_cache_gives_the_same_paths_as_the_canonical_world() {
            for seed in 1..=6 {
                let (w, mut rng) = random_world(seed);
                let grid = NavGrid::build(&w);
                assert!(grid.tile_count() > 1, "карта занимает несколько чанков");
                let none = PathOptions {
                    node_limit: 100_000,
                    ..Default::default()
                };
                same_result(&w, &grid, &mut rng, &none);
                let closed: HashSet<ObjectId> = w
                    .elements()
                    .filter(|e| matches!(e.kind, StructureKind::Opening(OpeningKind::Door)))
                    .map(|e| e.id)
                    .collect();
                let all_closed = PathOptions {
                    closed_doors: closed,
                    node_limit: 100_000,
                };
                same_result(&w, &grid, &mut rng, &all_closed);
            }
        }

        #[test]
        fn incremental_update_rebuilds_only_dirty_tiles_and_stays_correct() {
            let (mut w, mut rng) = random_world(99);
            w.take_dirty();
            let mut grid = NavGrid::build(&w);
            let total = grid.tile_count();
            let opts = PathOptions {
                node_limit: 100_000,
                ..Default::default()
            };
            let mut ever_partial = false;
            for round in 0..12u128 {
                // Случайная правка: убрать существующий элемент или поставить новую стену.
                if rng.range(2) == 0 {
                    let ids: Vec<ObjectId> = w.elements().map(|e| e.id).collect();
                    if !ids.is_empty() {
                        let id = ids[rng.range(ids.len() as u64) as usize];
                        w.apply(WorldCommand::RemoveElement { id }).unwrap();
                    }
                } else {
                    let (x, z) = (rng.range(70) - 5, rng.range(24) - 4);
                    let _ = w.apply(WorldCommand::PlaceElement(StructureElement {
                        id: ObjectId::from_u128(10_000 + round),
                        kind: StructureKind::Wall,
                        placement: Placement::Edge(GridEdge::along_x(x, z)),
                        floor: FloorId(0),
                        material: PrototypeId::new("t:m").unwrap(),
                        thickness: 0.2,
                    }));
                }
                let dirty = w.take_dirty();
                grid.apply_dirty(&w, &dirty);
                let rebuilt = grid.take_rebuilt();
                ever_partial |= rebuilt > 0 && rebuilt < total;
                assert!(rebuilt <= dirty.len());
                same_result(&w, &grid, &mut rng, &opts);
            }
            assert!(ever_partial, "перестраивалась только часть тайлов");
        }

        #[test]
        fn destroying_a_wall_updates_the_path_through_the_cache() {
            let mut w = world();
            room_with_door(&mut w, false);
            w.take_dirty();
            let mut grid = NavGrid::build(&w);
            let opts = PathOptions {
                node_limit: 5_000,
                ..Default::default()
            };
            assert_eq!(
                find_path_3d(&grid, (FloorId(0), OUTSIDE), (FloorId(0), INSIDE), &opts),
                Err(PathError::NoPath)
            );
            let id = w
                .edge_element(FloorId(0), GridEdge::along_x(1, 3))
                .unwrap()
                .id;
            w.apply(WorldCommand::RemoveElement { id }).unwrap();
            let dirty = w.take_dirty();
            grid.apply_dirty(&w, &dirty);
            assert!(grid.take_rebuilt() >= 1);
            let p =
                find_path_3d(&grid, (FloorId(0), OUTSIDE), (FloorId(0), INSIDE), &opts).unwrap();
            assert!(p.contains(&(FloorId(0), GridCell::new(1, 3))));
        }

        #[test]
        fn cache_follows_stairs_and_unknown_floors() {
            let mut w = world();
            w.apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(1),
                name: "up".into(),
                base_y: 3.0,
                height: 3.0,
            }))
            .unwrap();
            w.apply(WorldCommand::PlaceElement(StructureElement {
                id: ObjectId::from_u128(77),
                kind: StructureKind::Stair { to: FloorId(1) },
                placement: Placement::Run {
                    start: GridCell::new(5, 5),
                    dir: astrocore_math::Facing::South,
                    length: 3,
                },
                floor: FloorId(0),
                material: PrototypeId::new("t:m").unwrap(),
                thickness: 0.2,
            }))
            .unwrap();
            let grid = NavGrid::build(&w);
            let opts = PathOptions::default();
            let p = find_path_3d(
                &grid,
                (FloorId(0), GridCell::new(5, 3)),
                (FloorId(1), GridCell::new(7, 8)),
                &opts,
            )
            .unwrap();
            assert_eq!(
                p,
                find_path_3d(
                    &w,
                    (FloorId(0), GridCell::new(5, 3)),
                    (FloorId(1), GridCell::new(7, 8)),
                    &opts
                )
                .unwrap()
            );
            assert!(matches!(
                find_path_3d(
                    &grid,
                    (FloorId(9), GridCell::new(0, 0)),
                    (FloorId(1), GridCell::new(0, 0)),
                    &opts
                ),
                Err(PathError::UnknownFloor(_))
            ));
        }
    }
}
