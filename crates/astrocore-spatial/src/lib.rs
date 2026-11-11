use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use astrocore_core::{ObjectId, Revision};
use astrocore_math::{GridCell, GridEdge};
use astrocore_world::{FloorId, OpeningKind, StructureKind, World};
use serde::{Deserialize, Serialize};

/// Защита от запроса топологии для патологически большой области.
pub const MAX_TOPOLOGY_CELLS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoomId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomKind {
    /// Замкнутый стенами объём (двери и окна считаются проёмами).
    Interior,
    /// Всё, что соединено с внешней стороной. Одна на этаж.
    Exterior,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub kind: RoomKind,
    /// Ячейки комнаты. Для внешней области — только ячейки в пределах рассматриваемого окна.
    pub cells: BTreeSet<GridCell>,
}

impl Room {
    pub fn min_cell(&self) -> GridCell {
        *self.cells.iter().next().expect("комната не бывает пустой")
    }
}

/// Независимые каналы связи через портал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalChannels {
    pub movement: bool,
    pub sight: bool,
    pub sound: bool,
    pub air: bool,
    pub heat: bool,
}

impl PortalChannels {
    pub const OPEN: PortalChannels = PortalChannels {
        movement: true,
        sight: true,
        sound: true,
        air: true,
        heat: true,
    };
    pub const BLOCKED: PortalChannels = PortalChannels {
        movement: false,
        sight: false,
        sound: false,
        air: false,
        heat: false,
    };

    /// Каналы по умолчанию для вида проёма. Закрытая стеклянная створка не пускает
    /// движение и воздух, но пропускает обзор, звук и тепло.
    pub const fn for_opening(kind: OpeningKind, open: bool) -> PortalChannels {
        match (kind, open) {
            (OpeningKind::Door, true) => Self::OPEN,
            (OpeningKind::Door, false) => Self::BLOCKED,
            (OpeningKind::Window, true) => Self::OPEN,
            (OpeningKind::Window, false) => PortalChannels {
                movement: false,
                sight: true,
                sound: true,
                air: false,
                heat: true,
            },
        }
    }

    pub const fn passes(&self, channel: Channel) -> bool {
        match channel {
            Channel::Movement => self.movement,
            Channel::Sight => self.sight,
            Channel::Sound => self.sound,
            Channel::Air => self.air,
            Channel::Heat => self.heat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Movement,
    Sight,
    Sound,
    Air,
    Heat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portal {
    /// Проём — структурный элемент мира; дверь как сущность привязывается к нему.
    pub element: ObjectId,
    pub kind: OpeningKind,
    pub edge: GridEdge,
    pub a: RoomId,
    pub b: RoomId,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    #[error("область топологии слишком велика ({0} ячеек)")]
    TooLarge(usize),
    #[error("этаж {0:?} не определён")]
    UnknownFloor(FloorId),
}

/// Топология одного этажа на конкретной ревизии мира.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub floor: FloorId,
    /// Ревизия мира, по которой построена топология (`TopologyRevision`).
    pub revision: Revision,
    pub rooms: Vec<Room>,
    pub portals: Vec<Portal>,
    cell_room: HashMap<GridCell, RoomId>,
}

impl Topology {
    /// Построить топологию этажа. Для этажа без элементов комнат нет.
    pub fn build(world: &World, floor: FloorId) -> Result<Topology, TopologyError> {
        if world.floor(floor).is_none() {
            return Err(TopologyError::UnknownFloor(floor));
        }
        let mut topo = Topology {
            floor,
            revision: world.revision(),
            rooms: Vec::new(),
            portals: Vec::new(),
            cell_room: HashMap::new(),
        };
        let Some((lo, hi)) = world.floor_cell_bounds(floor) else {
            return Ok(topo);
        };
        // Окно с запасом в одну ячейку: кольцо вокруг построек всегда связно и
        // является «внешней» областью.
        let (lo, hi) = (lo.offset(-1, -1), hi.offset(1, 1));
        let width = (hi.x - lo.x + 1) as usize;
        let depth = (hi.z - lo.z + 1) as usize;
        let total = width.saturating_mul(depth);
        if total > MAX_TOPOLOGY_CELLS {
            return Err(TopologyError::TooLarge(total));
        }

        let exterior_seed = lo;
        let mut next_id = 0u32;
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                let seed = GridCell::new(x, z);
                if topo.cell_room.contains_key(&seed) {
                    continue;
                }
                let id = RoomId(next_id);
                next_id += 1;
                let cells = flood(world, floor, seed, (lo, hi));
                for c in &cells {
                    topo.cell_room.insert(*c, id);
                }
                let kind = if cells.contains(&exterior_seed) {
                    RoomKind::Exterior
                } else {
                    RoomKind::Interior
                };
                topo.rooms.push(Room { id, kind, cells });
            }
        }

        for e in world.elements().filter(|e| e.floor == floor) {
            if let (StructureKind::Opening(kind), astrocore_world::Placement::Edge(edge)) =
                (e.kind, e.placement)
            {
                let [c1, c2] = edge.adjacent_cells();
                if let (Some(&a), Some(&b)) = (topo.cell_room.get(&c1), topo.cell_room.get(&c2)) {
                    topo.portals.push(Portal {
                        element: e.id,
                        kind,
                        edge,
                        a,
                        b,
                    });
                }
            }
        }
        Ok(topo)
    }

    pub fn room(&self, id: RoomId) -> Option<&Room> {
        self.rooms.get(id.0 as usize)
    }

    pub fn room_at(&self, cell: GridCell) -> Option<RoomId> {
        self.cell_room.get(&cell).copied()
    }

    pub fn exterior(&self) -> Option<&Room> {
        self.rooms.iter().find(|r| r.kind == RoomKind::Exterior)
    }

    pub fn interior_rooms(&self) -> impl Iterator<Item = &Room> {
        self.rooms.iter().filter(|r| r.kind == RoomKind::Interior)
    }

    /// Комнаты, достижимые из `from` через порталы по заданному каналу.
    /// `is_open(element)` сообщает текущее состояние проёма (дверь открыта/закрыта).
    pub fn reachable(
        &self,
        from: RoomId,
        channel: Channel,
        is_open: impl Fn(ObjectId) -> bool,
    ) -> BTreeSet<RoomId> {
        let mut seen = BTreeSet::from([from]);
        let mut queue = VecDeque::from([from]);
        while let Some(r) = queue.pop_front() {
            for p in self.portals.iter().filter(|p| p.a == r || p.b == r) {
                if !PortalChannels::for_opening(p.kind, is_open(p.element)).passes(channel) {
                    continue;
                }
                let other = if p.a == r { p.b } else { p.a };
                if seen.insert(other) {
                    queue.push_back(other);
                }
            }
        }
        seen
    }

    /// Диагностика для валидатора карты в редакторе.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for p in &self.portals {
            if p.a == p.b {
                out.push(Diagnostic::PortalInsideSingleRoom { element: p.element });
            }
        }
        for room in self.interior_rooms() {
            let has_portal = self
                .portals
                .iter()
                .any(|p| p.a == room.id || p.b == room.id);
            if !has_portal {
                out.push(Diagnostic::SealedRoom {
                    room: room.id,
                    min_cell: room.min_cell(),
                });
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// Проём ведёт из комнаты в неё же — вероятно, лишний или недостроенная стена.
    PortalInsideSingleRoom { element: ObjectId },
    /// Замкнутая комната без единого прохода.
    SealedRoom { room: RoomId, min_cell: GridCell },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyChange {
    RoomCreated { min_cell: GridCell },
    RoomRemoved { min_cell: GridCell },
    RoomReshaped { min_cell: GridCell },
    PortalConnected { element: ObjectId },
    PortalDisconnected { element: ObjectId },
}

/// Различия двух топологий одного этажа. Комнаты сопоставляются по минимальной
/// ячейке: при росте комнаты «влево/вверх» она считается удалённой и созданной
/// заново — для инвалидации производных данных этого достаточно.
pub fn diff(old: &Topology, new: &Topology) -> Vec<TopologyChange> {
    let key = |t: &Topology| -> BTreeMap<GridCell, usize> {
        t.rooms
            .iter()
            .filter(|r| r.kind == RoomKind::Interior)
            .map(|r| (r.min_cell(), r.cells.len()))
            .collect()
    };
    let (a, b) = (key(old), key(new));
    let mut out = Vec::new();
    for (cell, size) in &a {
        match b.get(cell) {
            None => out.push(TopologyChange::RoomRemoved { min_cell: *cell }),
            Some(s) if s != size => out.push(TopologyChange::RoomReshaped { min_cell: *cell }),
            Some(_) => {}
        }
    }
    for cell in b.keys().filter(|c| !a.contains_key(c)) {
        out.push(TopologyChange::RoomCreated { min_cell: *cell });
    }
    let (pa, pb): (BTreeSet<ObjectId>, BTreeSet<ObjectId>) = (
        old.portals.iter().map(|p| p.element).collect(),
        new.portals.iter().map(|p| p.element).collect(),
    );
    out.extend(
        pb.difference(&pa)
            .map(|&element| TopologyChange::PortalConnected { element }),
    );
    out.extend(
        pa.difference(&pb)
            .map(|&element| TopologyChange::PortalDisconnected { element }),
    );
    out
}

fn flood(
    world: &World,
    floor: FloorId,
    seed: GridCell,
    bounds: (GridCell, GridCell),
) -> BTreeSet<GridCell> {
    let (lo, hi) = bounds;
    let inside = |c: GridCell| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z;
    let mut cells = BTreeSet::from([seed]);
    let mut queue = VecDeque::from([seed]);
    while let Some(c) = queue.pop_front() {
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = c.offset(dx, dz);
            if !inside(n) || cells.contains(&n) {
                continue;
            }
            let blocked = GridEdge::between(c, n)
                .is_some_and(|edge| world.edge_element(floor, edge).is_some());
            if !blocked {
                cells.insert(n);
                queue.push_back(n);
            }
        }
    }
    cells
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

    fn put(w: &mut World, id: u128, kind: StructureKind, edge: GridEdge) -> ObjectId {
        let oid = ObjectId::from_u128(id);
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: oid,
            kind,
            placement: Placement::Edge(edge),
            floor: FloorId(0),
            material: PrototypeId::new("t:m").unwrap(),
            thickness: 0.2,
        }))
        .unwrap();
        oid
    }

    /// Комната 3×3 ячейки (0..3, 0..3): 12 рёбер периметра. Возвращает ID стен по рёбрам.
    fn square_room(w: &mut World) -> Vec<(GridEdge, ObjectId)> {
        let mut edges = Vec::new();
        for i in 0..3 {
            edges.push(GridEdge::along_x(i, 0)); // север (z=0)
            edges.push(GridEdge::along_x(i, 3)); // юг (z=3)
            edges.push(GridEdge::along_z(0, i)); // запад
            edges.push(GridEdge::along_z(3, i)); // восток
        }
        edges
            .into_iter()
            .enumerate()
            .map(|(n, e)| (e, put(w, 100 + n as u128, StructureKind::Wall, e)))
            .collect()
    }

    #[test]
    fn closed_room_is_interior_and_outside_is_exterior() {
        let mut w = world();
        square_room(&mut w);
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(t.rooms.len(), 2);
        let inside = t.room_at(GridCell::new(1, 1)).unwrap();
        let outside = t.room_at(GridCell::new(-1, -1)).unwrap();
        assert_ne!(inside, outside);
        assert_eq!(t.room(inside).unwrap().kind, RoomKind::Interior);
        assert_eq!(t.room(inside).unwrap().cells.len(), 9);
        assert_eq!(t.room(outside).unwrap().kind, RoomKind::Exterior);
        assert!(t.portals.is_empty());
    }

    #[test]
    fn door_creates_portal_between_room_and_exterior() {
        let mut w = world();
        let walls = square_room(&mut w);
        let (door_edge, wall_id) = walls[0];
        w.apply(WorldCommand::RemoveElement { id: wall_id })
            .unwrap();
        let door = put(
            &mut w,
            900,
            StructureKind::Opening(OpeningKind::Door),
            door_edge,
        );
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(
            t.rooms.len(),
            2,
            "дверь — проём, комната остаётся замкнутой"
        );
        assert_eq!(t.portals.len(), 1);
        let p = &t.portals[0];
        assert_eq!(p.element, door);
        let inside = t.room_at(GridCell::new(1, 1)).unwrap();
        assert!(p.a == inside || p.b == inside);
        assert!(t.diagnostics().is_empty());
    }

    #[test]
    fn destroying_a_wall_merges_room_with_exterior_and_reports_changes() {
        let mut w = world();
        let walls = square_room(&mut w);
        let before = Topology::build(&w, FloorId(0)).unwrap();
        w.apply(WorldCommand::RemoveElement { id: walls[5].1 })
            .unwrap();
        let after = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(after.rooms.len(), 1);
        assert!(after.interior_rooms().next().is_none());
        assert!(after.revision > before.revision);
        assert_eq!(
            diff(&before, &after),
            vec![TopologyChange::RoomRemoved {
                min_cell: GridCell::new(0, 0)
            }]
        );
    }

    #[test]
    fn partition_wall_splits_room() {
        let mut w = world();
        square_room(&mut w);
        for (n, z) in (0..3).enumerate() {
            put(
                &mut w,
                500 + n as u128,
                StructureKind::Wall,
                GridEdge::along_z(1, z),
            );
        }
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(t.interior_rooms().count(), 2);
        assert_ne!(
            t.room_at(GridCell::new(0, 1)),
            t.room_at(GridCell::new(2, 1))
        );
    }

    #[test]
    fn portal_channels_are_independent() {
        let w = PortalChannels::for_opening(OpeningKind::Window, false);
        assert!(!w.passes(Channel::Movement) && !w.passes(Channel::Air));
        assert!(w.passes(Channel::Sight) && w.passes(Channel::Sound));
        assert_eq!(
            PortalChannels::for_opening(OpeningKind::Door, false),
            PortalChannels::BLOCKED
        );
        assert_eq!(
            PortalChannels::for_opening(OpeningKind::Door, true),
            PortalChannels::OPEN
        );
    }

    #[test]
    fn reachability_respects_door_state_and_channel() {
        let mut w = world();
        let walls = square_room(&mut w);
        let (edge, id) = walls[0];
        w.apply(WorldCommand::RemoveElement { id }).unwrap();
        let door = put(&mut w, 900, StructureKind::Opening(OpeningKind::Door), edge);
        let t = Topology::build(&w, FloorId(0)).unwrap();
        let inside = t.room_at(GridCell::new(1, 1)).unwrap();
        assert_eq!(t.reachable(inside, Channel::Movement, |_| false).len(), 1);
        assert_eq!(
            t.reachable(inside, Channel::Movement, |e| e == door).len(),
            2
        );
    }

    #[test]
    fn window_passes_sight_when_closed_but_not_movement() {
        let mut w = world();
        let walls = square_room(&mut w);
        let (edge, id) = walls[0];
        w.apply(WorldCommand::RemoveElement { id }).unwrap();
        put(
            &mut w,
            901,
            StructureKind::Opening(OpeningKind::Window),
            edge,
        );
        let t = Topology::build(&w, FloorId(0)).unwrap();
        let inside = t.room_at(GridCell::new(1, 1)).unwrap();
        assert_eq!(t.reachable(inside, Channel::Sight, |_| false).len(), 2);
        assert_eq!(t.reachable(inside, Channel::Movement, |_| false).len(), 1);
        assert_eq!(t.reachable(inside, Channel::Air, |_| false).len(), 1);
    }

    #[test]
    fn sealed_room_is_diagnosed() {
        let mut w = world();
        square_room(&mut w);
        let d = Topology::build(&w, FloorId(0)).unwrap().diagnostics();
        assert!(matches!(d.as_slice(), [Diagnostic::SealedRoom { .. }]));
    }

    #[test]
    fn door_in_open_space_is_diagnosed() {
        let mut w = world();
        put(
            &mut w,
            1,
            StructureKind::Opening(OpeningKind::Door),
            GridEdge::along_x(0, 0),
        );
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(
            t.diagnostics(),
            vec![Diagnostic::PortalInsideSingleRoom {
                element: ObjectId::from_u128(1)
            }]
        );
    }

    #[test]
    fn empty_and_unknown_floors() {
        let w = world();
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert!(t.rooms.is_empty() && t.portals.is_empty() && t.exterior().is_none());
        assert_eq!(
            Topology::build(&w, FloorId(5)),
            Err(TopologyError::UnknownFloor(FloorId(5)))
        );
    }

    #[test]
    fn huge_area_is_refused() {
        let mut w = world();
        put(&mut w, 1, StructureKind::Wall, GridEdge::along_x(0, 0));
        put(
            &mut w,
            2,
            StructureKind::Wall,
            GridEdge::along_x(100_000, 100_000),
        );
        assert!(matches!(
            Topology::build(&w, FloorId(0)),
            Err(TopologyError::TooLarge(_))
        ));
    }

    #[test]
    fn negative_coordinates_work() {
        let mut w = world();
        for (n, e) in [
            GridEdge::along_x(-3, -3),
            GridEdge::along_x(-3, -2),
            GridEdge::along_z(-3, -3),
            GridEdge::along_z(-2, -3),
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut w, n as u128, StructureKind::Wall, e);
        }
        let t = Topology::build(&w, FloorId(0)).unwrap();
        assert_eq!(t.interior_rooms().count(), 1);
        assert_eq!(
            t.room(t.room_at(GridCell::new(-3, -3)).unwrap())
                .unwrap()
                .kind,
            RoomKind::Interior
        );
    }
}
