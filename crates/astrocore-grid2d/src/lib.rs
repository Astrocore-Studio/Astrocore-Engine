pub mod session;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub z: i16,
}

impl Cell {
    pub const fn new(x: i32, y: i32) -> Self {
        Self::at(x, y, 0)
    }

    pub const fn at(x: i32, y: i32, z: i16) -> Self {
        Self { x, y, z }
    }

    pub fn distance_on_level(self, other: Self) -> Option<u32> {
        (self.z == other.z).then(|| {
            self.x
                .abs_diff(other.x)
                .saturating_add(self.y.abs_diff(other.y))
        })
    }

    pub fn center(self) -> Position {
        Position {
            x: self.x as f32 + 0.5,
            y: self.y as f32 + 0.5,
            z: self.z,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tile {
    Void,
    Floor,
    Wall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "GridData")]
pub struct Grid {
    levels: BTreeMap<i16, GridLevel>,
}

#[derive(Deserialize)]
struct GridData {
    levels: BTreeMap<i16, GridLevel>,
}

impl TryFrom<GridData> for Grid {
    type Error = &'static str;

    fn try_from(data: GridData) -> Result<Self, Self::Error> {
        Self::from_levels(data.levels)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "LevelData")]
pub struct GridLevel {
    width: u16,
    height: u16,
    tiles: Vec<Tile>,
}

#[derive(Deserialize)]
struct LevelData {
    width: u16,
    height: u16,
    tiles: Vec<Tile>,
}

impl TryFrom<LevelData> for GridLevel {
    type Error = &'static str;

    fn try_from(data: LevelData) -> Result<Self, Self::Error> {
        Self::new(data.width, data.height, data.tiles)
    }
}

impl GridLevel {
    /// Ряды идут от нижнего края к верхнему, клетки в ряду — слева направо.
    pub fn new(width: u16, height: u16, tiles: Vec<Tile>) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || tiles.len() != usize::from(width) * usize::from(height) {
            return Err("размеры сетки не соответствуют числу тайлов");
        }
        Ok(Self {
            width,
            height,
            tiles,
        })
    }

    fn index(&self, cell: Cell) -> Option<usize> {
        let x = u16::try_from(cell.x).ok()?;
        let y = u16::try_from(cell.y).ok()?;
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    fn cells(&self, z: i16) -> impl Iterator<Item = (Cell, Tile)> + '_ {
        let width = usize::from(self.width);
        self.tiles
            .iter()
            .copied()
            .enumerate()
            .map(move |(i, tile)| (Cell::at((i % width) as i32, (i / width) as i32, z), tile))
    }
}

impl Grid {
    /// Сетка с одним уровнем Z = 0.
    pub fn new(width: u16, height: u16, tiles: Vec<Tile>) -> Result<Self, &'static str> {
        Self::from_levels(BTreeMap::from([(0, GridLevel::new(width, height, tiles)?)]))
    }

    pub fn from_levels(levels: BTreeMap<i16, GridLevel>) -> Result<Self, &'static str> {
        if !levels.contains_key(&0) {
            return Err("в сетке должен быть базовый уровень Z = 0");
        }
        Ok(Self { levels })
    }

    pub fn size(&self) -> (u16, u16) {
        let base = &self.levels[&0];
        (base.width, base.height)
    }

    pub fn size_at(&self, z: i16) -> Option<(u16, u16)> {
        self.levels.get(&z).map(|level| (level.width, level.height))
    }

    pub fn levels(&self) -> impl DoubleEndedIterator<Item = i16> + '_ {
        self.levels.keys().copied()
    }

    pub fn tile(&self, cell: Cell) -> Tile {
        let Some(level) = self.levels.get(&cell.z) else {
            return Tile::Void;
        };
        level
            .index(cell)
            .map_or(Tile::Void, |index| level.tiles[index])
    }

    pub fn contains(&self, cell: Cell) -> bool {
        self.levels
            .get(&cell.z)
            .is_some_and(|level| level.index(cell).is_some())
    }

    pub fn set_tile(&mut self, cell: Cell, tile: Tile) -> Option<Tile> {
        let level = self.levels.get_mut(&cell.z)?;
        let index = level.index(cell)?;
        Some(std::mem::replace(&mut level.tiles[index], tile))
    }

    pub fn cells_at(&self, z: i16) -> impl Iterator<Item = (Cell, Tile)> + '_ {
        self.levels
            .get(&z)
            .into_iter()
            .flat_map(move |level| level.cells(z))
    }

    pub fn cells(&self) -> impl Iterator<Item = (Cell, Tile)> + '_ {
        self.levels.iter().flat_map(|(&z, level)| level.cells(z))
    }

    pub fn move_body(
        &self,
        position: Position,
        movement: [i8; 2],
        blocked: impl Fn(Cell) -> bool,
    ) -> Position {
        self.move_body_at_speed(position, movement, 1.0, blocked)
    }

    pub fn move_body_at_speed(
        &self,
        position: Position,
        movement: [i8; 2],
        speed_ratio: f32,
        blocked: impl Fn(Cell) -> bool,
    ) -> Position {
        let [x, y] = movement.map(|axis| f32::from(axis.clamp(-1, 1)));
        let scale = WALK_STEP * speed_ratio.clamp(0.0, 1.0) / x.hypot(y).max(1.0);
        self.slide_body(position, [x * scale, y * scale], blocked)
    }

    /// Сдвиг тела к другому на том же этаже с заданным зазором и обычной проверкой коллизий.
    pub fn move_body_towards(
        &self,
        position: Position,
        target: Position,
        spacing: f32,
        blocked: impl Fn(Cell) -> bool,
    ) -> Position {
        let dx = target.x - position.x;
        let dy = target.y - position.y;
        let distance = dx.hypot(dy);
        if position.z != target.z || !distance.is_finite() || distance <= spacing.max(0.0) {
            return position;
        }
        let scale = (distance - spacing.max(0.0)).min(WALK_STEP) / distance;
        self.slide_body(position, [dx * scale, dy * scale], blocked)
    }

    fn slide_body(
        &self,
        position: Position,
        [dx, dy]: [f32; 2],
        blocked: impl Fn(Cell) -> bool,
    ) -> Position {
        let mut result = position;
        for (dx, dy) in [(dx, 0.0), (0.0, dy)] {
            // Оси проверяются по очереди для скольжения вдоль стены.
            let next = Position {
                x: result.x + dx,
                y: result.y + dy,
                z: result.z,
            };
            if next
                .covered_cells()
                .all(|cell| self.tile(cell) == Tile::Floor && !blocked(cell))
            {
                result = next;
            }
        }
        result
    }
}

/// Положение в непрерывных координатах сетки, одна единица равна одному тайлу.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub z: i16,
}

pub const BODY_RADIUS: f32 = 0.27;
const WALK_SPEED: f32 = 3.4;
const WALK_STEP: f32 = WALK_SPEED / session::TICK_HZ as f32;

impl Position {
    pub fn cell(self) -> Cell {
        Cell::at(self.x.floor() as i32, self.y.floor() as i32, self.z)
    }

    pub fn overlaps(self, cell: Cell) -> bool {
        self.covered_cells().any(|covered| covered == cell)
    }

    fn covered_cells(self) -> impl Iterator<Item = Cell> {
        let min = Position {
            x: self.x - BODY_RADIUS,
            y: self.y - BODY_RADIUS,
            z: self.z,
        }
        .cell();
        let max = Position {
            x: self.x + BODY_RADIUS,
            y: self.y + BODY_RADIUS,
            z: self.z,
        }
        .cell();
        (min.y..=max.y).flat_map(move |y| (min.x..=max.x).map(move |x| Cell::at(x, y, self.z)))
    }
}
