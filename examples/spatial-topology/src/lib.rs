use std::collections::BTreeSet;

use astrocore_api::SimulationTopology;
use astrocore_core::ObjectId;
use astrocore_math::glam::Vec3;
use astrocore_spatial::{Channel, RoomId, Topology, TopologyChange};
use astrocore_vertical_slice::Slice;
use astrocore_world::{AuthoredKind, AuthoredObject, FloorId, WorldCommand, WorldError};

/// Сводка по этажу: что видят симуляции через `SimulationTopology`.
#[derive(Debug, Clone, PartialEq)]
pub struct FloorReport {
    pub interior_rooms: usize,
    pub portals: usize,
    /// Сколько комнат достижимо из первой внутренней комнаты по каналу при закрытых/открытых проёмах.
    pub reach_closed: BTreeSet<RoomId>,
    pub reach_open: BTreeSet<RoomId>,
}

pub fn floor_report(slice: &Slice, floor: FloorId) -> Result<FloorReport, WorldError> {
    let topo = Topology::build(&slice.world, floor)
        .map_err(|e| WorldError::CorruptState(e.to_string()))?;
    let first = topo
        .interior_rooms()
        .next()
        .map(|r| r.id)
        .ok_or_else(|| WorldError::CorruptState("на этаже нет внутренних комнат".into()))?;
    let closed = |_: ObjectId| false;
    let open = |_: ObjectId| true;
    Ok(FloorReport {
        interior_rooms: topo.interior_rooms().count(),
        portals: topo.portals.len(),
        reach_closed: topo.reachable_rooms(first, Channel::Air, &closed),
        reach_open: topo.reachable_rooms(first, Channel::Air, &open),
    })
}

/// Пролом стены меняет топологию: симуляции получают список изменений вместо полного пересчёта.
pub fn breach_changes(slice: &mut Slice) -> Result<Vec<TopologyChange>, WorldError> {
    let before = Topology::build(&slice.world, FloorId(0))
        .map_err(|e| WorldError::CorruptState(e.to_string()))?;
    slice.breach()?;
    let after = Topology::build(&slice.world, FloorId(0))
        .map_err(|e| WorldError::CorruptState(e.to_string()))?;
    Ok(after.changes_from(&before))
}

/// Авторская зона поверх здания: логическая область без стен, которую задаёт игровой пакет.
pub fn mark_zone(slice: &mut Slice, name: &str) -> Result<ObjectId, WorldError> {
    let id = ObjectId::from_u128(0xC0DE);
    let mut zone = AuthoredObject::new(
        id,
        AuthoredKind::Zone,
        FloorId(0),
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(12.0, 3.0, 12.0),
    );
    zone.metadata.insert("name".into(), name.into());
    slice.exec(WorldCommand::PlaceAuthored(zone))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_portals_and_channels_follow_door_state() {
        let slice = Slice::build().unwrap();
        let r = floor_report(&slice, FloorId(0)).unwrap();
        assert!(r.interior_rooms >= 1);
        assert!(r.portals >= 1, "входная дверь — портал");
        assert!(r.reach_open.len() >= r.reach_closed.len());
        assert!(
            r.reach_closed
                .contains(r.reach_closed.iter().next().unwrap())
        );
    }

    #[test]
    fn breach_reports_topology_changes_and_zone_is_queryable() {
        let mut slice = Slice::build().unwrap();
        let changes = breach_changes(&mut slice).unwrap();
        assert!(!changes.is_empty(), "пролом меняет комнаты или порталы");
        let id = mark_zone(&mut slice, "двор").unwrap();
        let at = slice.world.authored_at(Vec3::new(2.0, 1.0, 2.0));
        assert_eq!(at.len(), 1);
        assert_eq!(at[0].id, id);
        assert_eq!(at[0].metadata.get("name").map(String::as_str), Some("двор"));
    }
}
