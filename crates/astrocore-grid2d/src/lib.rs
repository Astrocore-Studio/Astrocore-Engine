pub mod session;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
}

impl Cell {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub fn center(self) -> Position {
        Position {
            x: self.x as f32 + 0.5,
            y: self.y as f32 + 0.5,
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
    width: u16,
    height: u16,
    tiles: Vec<Tile>,
}

#[derive(Deserialize)]
struct GridData {
    width: u16,
    height: u16,
    tiles: Vec<Tile>,
}

impl TryFrom<GridData> for Grid {
    type Error = &'static str;

    fn try_from(data: GridData) -> Result<Self, Self::Error> {
        Self::new(data.width, data.height, data.tiles)
    }
}

impl Grid {
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

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn tile(&self, cell: Cell) -> Tile {
        self.index(cell)
            .map_or(Tile::Void, |index| self.tiles[index])
    }

    pub fn contains(&self, cell: Cell) -> bool {
        self.index(cell).is_some()
    }

    pub fn set_tile(&mut self, cell: Cell, tile: Tile) -> Option<Tile> {
        let index = self.index(cell)?;
        Some(std::mem::replace(&mut self.tiles[index], tile))
    }

    fn index(&self, cell: Cell) -> Option<usize> {
        let x = u16::try_from(cell.x).ok()?;
        let y = u16::try_from(cell.y).ok()?;
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    pub fn cells(&self) -> impl Iterator<Item = (Cell, Tile)> + '_ {
        let width = usize::from(self.width);
        self.tiles
            .iter()
            .copied()
            .enumerate()
            .map(move |(i, tile)| (Cell::new((i % width) as i32, (i / width) as i32), tile))
    }

    pub fn move_body(
        &self,
        position: Position,
        movement: [i8; 2],
        blocked: impl Fn(Cell) -> bool,
    ) -> Position {
        let [x, y] = movement.map(|axis| f32::from(axis.clamp(-1, 1)));
        let distance = WALK_SPEED / session::TICK_HZ as f32;
        let scale = distance / x.hypot(y).max(1.0);
        let mut result = position;
        for (dx, dy) in [(x * scale, 0.0), (0.0, y * scale)] {
            // Оси проверяются по очереди для скольжения вдоль стены.
            let next = Position {
                x: result.x + dx,
                y: result.y + dy,
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
}

pub const BODY_RADIUS: f32 = 0.27;
const WALK_SPEED: f32 = 3.4;

impl Position {
    pub fn cell(self) -> Cell {
        Cell::new(self.x.floor() as i32, self.y.floor() as i32)
    }

    pub fn overlaps(self, cell: Cell) -> bool {
        self.covered_cells().any(|covered| covered == cell)
    }

    fn covered_cells(self) -> impl Iterator<Item = Cell> {
        let min = Position {
            x: self.x - BODY_RADIUS,
            y: self.y - BODY_RADIUS,
        }
        .cell();
        let max = Position {
            x: self.x + BODY_RADIUS,
            y: self.y + BODY_RADIUS,
        }
        .cell();
        (min.y..=max.y).flat_map(move |y| (min.x..=max.x).map(move |x| Cell::new(x, y)))
    }
}
