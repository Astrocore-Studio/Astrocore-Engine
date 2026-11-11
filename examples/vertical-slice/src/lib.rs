use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{GridCell, GridEdge};
use astrocore_world::{
    FloorDef, FloorId, FreePlacement, OpeningKind, Placement, SlotKey, StructureElement,
    StructureKind, World, WorldCommand, WorldConfig, WorldError, WorldOp,
};

/// Первая и последняя клетка здания по X/Z (6×6 клеток), пересекает границу чанков x = 32.
pub const MIN_X: i32 = 29;
pub const MAX_X: i32 = 35;
pub const MIN_Z: i32 = 0;
pub const MAX_Z: i32 = 6;
pub const FLOORS: i32 = 3;
pub const FLOOR_HEIGHT_M: f32 = 4.0;
/// Ребро входной двери на первом этаже.
pub const DOOR_EDGE: GridEdge = GridEdge::along_x(31, MIN_Z);
/// Лестницы: из `(MIN_X + 1, z)` вдоль +X длиной [`STAIR_LENGTH`], выход — за маршем.
pub const STAIR_LENGTH: u8 = 4;
/// Ряды клеток (Z), по которым идут лестницы с 1-го на 2-й и со 2-го на 3-й этаж.
pub const STAIR_ROW_0: i32 = 3;
pub const STAIR_ROW_1: i32 = 4;
/// Участок пола перед входом (клетки по Z = −3..−1), чтобы было куда выйти из двери.
pub const APRON_Z: std::ops::RangeInclusive<i32> = -3..=-1;
/// Ребро стены, которую «разрушает» сценарий.
pub const BREACH_EDGE: GridEdge = GridEdge::along_z(MAX_X, 2);

pub fn material() -> PrototypeId {
    PrototypeId::new("slice:concrete").expect("валидный ID")
}

/// Построитель: применяет команды к серверному миру и ведёт журнал операций.
pub struct Slice {
    pub world: World,
    pub log: Vec<WorldOp>,
    pub door: ObjectId,
    next_id: u128,
}

impl Slice {
    pub fn build() -> Result<Slice, WorldError> {
        let world = World::new("vertical-slice", WorldConfig::default())?;
        let mut s = Slice {
            world,
            log: Vec::new(),
            door: ObjectId::from_u128(0),
            next_id: 1,
        };
        for f in 0..FLOORS {
            s.exec(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(f),
                name: format!("floor-{f}"),
                base_y: f as f32 * FLOOR_HEIGHT_M,
                height: FLOOR_HEIGHT_M,
            }))?;
        }
        for f in 0..FLOORS {
            s.ring_of_walls(FloorId(f))?;
            s.slabs(FloorId(f))?;
        }
        // Входная дверь: на ребре первого этажа стену заменяет проём.
        let wall = s
            .world
            .edge_element(FloorId(0), DOOR_EDGE)
            .map(|e| e.id)
            .expect("стена на месте двери");
        s.exec(WorldCommand::RemoveElement { id: wall })?;
        s.door = s.place(
            FloorId(0),
            StructureKind::Opening(OpeningKind::Door),
            Placement::Edge(DOOR_EDGE),
        )?;
        s.apron()?;
        s.stairs()?;
        s.statue()?;
        Ok(s)
    }

    pub fn exec(&mut self, command: WorldCommand) -> Result<WorldOp, WorldError> {
        let op = self.world.apply(command)?;
        self.log.push(op.clone());
        Ok(op)
    }

    fn place(
        &mut self,
        floor: FloorId,
        kind: StructureKind,
        placement: Placement,
    ) -> Result<ObjectId, WorldError> {
        let id = ObjectId::from_u128(self.next_id);
        self.next_id += 1;
        self.exec(WorldCommand::PlaceElement(StructureElement {
            id,
            kind,
            placement,
            floor,
            material: material(),
            thickness: 0.25,
        }))?;
        Ok(id)
    }

    fn ring_of_walls(&mut self, floor: FloorId) -> Result<(), WorldError> {
        for x in MIN_X..MAX_X {
            self.place(
                floor,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(x, MIN_Z)),
            )?;
            self.place(
                floor,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_x(x, MAX_Z)),
            )?;
        }
        for z in MIN_Z..MAX_Z {
            self.place(
                floor,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(MIN_X, z)),
            )?;
            self.place(
                floor,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(MAX_X, z)),
            )?;
        }
        Ok(())
    }

    fn slabs(&mut self, floor: FloorId) -> Result<(), WorldError> {
        for x in MIN_X..MAX_X {
            for z in MIN_Z..MAX_Z {
                let cell = astrocore_math::GridCell::new(x, z);
                self.place(floor, StructureKind::Floor, Placement::Cell(cell))?;
            }
        }
        Ok(())
    }

    /// Площадка перед дверью.
    fn apron(&mut self) -> Result<(), WorldError> {
        for x in (MIN_X - 1)..=MAX_X {
            for z in APRON_Z {
                self.place(
                    FloorId(0),
                    StructureKind::Floor,
                    Placement::Cell(GridCell::new(x, z)),
                )?;
            }
        }
        Ok(())
    }

    /// Две лестницы: 1→2 этаж и 2→3 этаж. Над маршем убираются перекрытия верхнего этажа.
    fn stairs(&mut self) -> Result<(), WorldError> {
        for (from, row) in [(0, STAIR_ROW_0), (1, STAIR_ROW_1)] {
            let to = FloorId(from + 1);
            let start = GridCell::new(MIN_X + 1, row);
            for i in 0..i32::from(STAIR_LENGTH) {
                let cell = start.step(astrocore_math::Facing::East, i);
                if let Some(slab) = self
                    .world
                    .element_at(to, SlotKey::FloorSlab(cell))
                    .map(|e| e.id)
                {
                    self.exec(WorldCommand::RemoveElement { id: slab })?;
                }
            }
            self.place(
                FloorId(from),
                StructureKind::Stair { to },
                Placement::Run {
                    start,
                    dir: astrocore_math::Facing::East,
                    length: STAIR_LENGTH,
                },
            )?;
        }
        Ok(())
    }

    /// Свободная модель в холле: статуя (GLB подключает билд; мир хранит габаритный ящик).
    fn statue(&mut self) -> Result<(), WorldError> {
        let id = ObjectId::from_u128(self.next_id);
        self.next_id += 1;
        self.exec(WorldCommand::PlaceElement(StructureElement {
            id,
            kind: StructureKind::Freeform,
            placement: Placement::Free(FreePlacement::from_position(
                [(MIN_X + 4) as f32 + 0.5, 0.25, 1.5],
                1.0,
                45.0,
                [0.35, 0.9, 0.35],
            )),
            floor: FloorId(0),
            material: PrototypeId::new("slice:statue").expect("валидный ID"),
            thickness: 0.1,
        }))?;
        Ok(())
    }

    /// Разрушить стену на первом этаже (ребро [`BREACH_EDGE`]).
    pub fn breach(&mut self) -> Result<WorldOp, WorldError> {
        let id = self
            .world
            .edge_element(FloorId(0), BREACH_EDGE)
            .map(|e| e.id)
            .ok_or(WorldError::InvalidPlacement(
                "на месте пролома нет стены".into(),
            ))?;
        self.exec(WorldCommand::RemoveElement { id })
    }
}
