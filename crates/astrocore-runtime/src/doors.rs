use std::collections::{BTreeMap, BTreeSet, HashSet};

use astrocore_core::ObjectId;
use astrocore_math::glam::Vec3;
use astrocore_spatial::PortalChannels;
use astrocore_world::{OpeningKind, Placement, StructureKind, World};

/// Дальность взаимодействия по умолчанию, метры.
pub const DEFAULT_REACH_M: f32 = 2.5;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DoorState {
    pub open: bool,
    pub locked: bool,
    /// Метка доступа, которую обязан иметь актор (например, «keycard.blue»). Смысл метки
    /// задаёт игра, движок лишь сравнивает строки.
    pub required_access: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorEvent {
    Opened(ObjectId),
    Closed(ObjectId),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InteractError {
    #[error("объект {0} не является дверью")]
    NotADoor(ObjectId),
    #[error("слишком далеко: {distance:.2} м при дальности {reach:.2} м")]
    OutOfReach { distance: f32, reach: f32 },
    #[error("заперто")]
    Locked,
    #[error("нет требуемого доступа {0:?}")]
    AccessDenied(String),
    #[error("неверные координаты актора")]
    BadPosition,
}

/// Запрос на взаимодействие. Приходит от клиента и считается недоверенным: позицию
/// актора сервер берёт из собственной симуляции, а не из сообщения.
pub struct InteractRequest<'a> {
    /// Позиция актора, известная серверу.
    pub actor_pos: Vec3,
    pub target: ObjectId,
    pub access: &'a BTreeSet<String>,
    pub reach_m: f32,
}

#[derive(Debug, Clone, Default)]
pub struct DoorRegistry {
    doors: BTreeMap<ObjectId, DoorState>,
}

impl DoorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Привести набор дверей в соответствие с миром: новые проёмы получают закрытую
    /// дверь, исчезнувшие — удаляются. Состояние существующих сохраняется.
    pub fn sync(&mut self, world: &World) {
        let present: HashSet<ObjectId> = world
            .elements()
            .filter(|e| matches!(e.kind, StructureKind::Opening(OpeningKind::Door)))
            .map(|e| e.id)
            .collect();
        self.doors.retain(|id, _| present.contains(id));
        for id in present {
            self.doors.entry(id).or_default();
        }
    }

    pub fn get(&self, id: ObjectId) -> Option<&DoorState> {
        self.doors.get(&id)
    }

    pub fn get_mut(&mut self, id: ObjectId) -> Option<&mut DoorState> {
        self.doors.get_mut(&id)
    }

    pub fn is_open(&self, id: ObjectId) -> bool {
        self.doors.get(&id).is_some_and(|d| d.open)
    }

    /// Закрытые двери (для навигации).
    pub fn closed_set(&self) -> HashSet<ObjectId> {
        self.doors
            .iter()
            .filter(|(_, d)| !d.open)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Текущие каналы портала двери.
    pub fn channels(&self, id: ObjectId) -> PortalChannels {
        PortalChannels::for_opening(OpeningKind::Door, self.is_open(id))
    }

    /// Серверная обработка взаимодействия: переключает дверь, если актор рядом и имеет доступ.
    pub fn interact(
        &mut self,
        world: &World,
        req: &InteractRequest<'_>,
    ) -> Result<DoorEvent, InteractError> {
        if !req.actor_pos.is_finite() {
            return Err(InteractError::BadPosition);
        }
        let element = world
            .element(req.target)
            .filter(|e| matches!(e.kind, StructureKind::Opening(OpeningKind::Door)))
            .ok_or(InteractError::NotADoor(req.target))?;
        let floor = world
            .floor(element.floor)
            .ok_or(InteractError::NotADoor(req.target))?;
        let Placement::Edge(edge) = element.placement else {
            return Err(InteractError::NotADoor(req.target));
        };
        let (a, b) = edge.endpoints(world.config().grid_step_m, floor.base_y);
        let door_center = (a + b) * 0.5 + Vec3::new(0.0, floor.height * 0.5, 0.0);
        let distance = req.actor_pos.distance(door_center);
        if distance > req.reach_m {
            return Err(InteractError::OutOfReach {
                distance,
                reach: req.reach_m,
            });
        }
        let state = self.doors.entry(req.target).or_default();
        if state.locked {
            return Err(InteractError::Locked);
        }
        if let Some(need) = &state.required_access
            && !req.access.contains(need)
        {
            return Err(InteractError::AccessDenied(need.clone()));
        }
        state.open = !state.open;
        Ok(if state.open {
            DoorEvent::Opened(req.target)
        } else {
            DoorEvent::Closed(req.target)
        })
    }
}

#[cfg(test)]
mod tests {
    use astrocore_core::PrototypeId;
    use astrocore_math::GridEdge;
    use astrocore_world::{FloorDef, FloorId, StructureElement, WorldCommand, WorldConfig};

    use super::*;

    const DOOR: ObjectId = ObjectId::from_u128(1);

    fn world() -> World {
        let mut w = World::new("t", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 0.0,
            height: 3.0,
        }))
        .unwrap();
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: DOOR,
            kind: StructureKind::Opening(OpeningKind::Door),
            placement: Placement::Edge(GridEdge::along_x(4, 0)),
            floor: FloorId(0),
            material: PrototypeId::new("t:door").unwrap(),
            thickness: 0.1,
        }))
        .unwrap();
        w
    }

    fn near() -> Vec3 {
        Vec3::new(4.5, 1.0, 1.0)
    }

    fn req<'a>(pos: Vec3, access: &'a BTreeSet<String>) -> InteractRequest<'a> {
        InteractRequest {
            actor_pos: pos,
            target: DOOR,
            access,
            reach_m: DEFAULT_REACH_M,
        }
    }

    #[test]
    fn door_toggles_when_actor_is_near() {
        let w = world();
        let mut doors = DoorRegistry::new();
        doors.sync(&w);
        let none = BTreeSet::new();
        assert!(!doors.is_open(DOOR));
        assert_eq!(doors.closed_set(), HashSet::from([DOOR]));
        assert_eq!(
            doors.interact(&w, &req(near(), &none)),
            Ok(DoorEvent::Opened(DOOR))
        );
        assert!(doors.is_open(DOOR) && doors.closed_set().is_empty());
        assert_eq!(doors.channels(DOOR), PortalChannels::OPEN);
        assert_eq!(
            doors.interact(&w, &req(near(), &none)),
            Ok(DoorEvent::Closed(DOOR))
        );
        assert_eq!(doors.channels(DOOR), PortalChannels::BLOCKED);
    }

    #[test]
    fn far_actors_and_bad_input_are_refused_without_state_change() {
        let w = world();
        let mut doors = DoorRegistry::new();
        doors.sync(&w);
        let none = BTreeSet::new();
        let far = Vec3::new(40.0, 1.0, 40.0);
        assert!(matches!(
            doors.interact(&w, &req(far, &none)),
            Err(InteractError::OutOfReach { .. })
        ));
        assert_eq!(
            doors.interact(&w, &req(Vec3::new(f32::NAN, 0.0, 0.0), &none)),
            Err(InteractError::BadPosition)
        );
        let wall_target = InteractRequest {
            actor_pos: near(),
            target: ObjectId::from_u128(404),
            access: &none,
            reach_m: 5.0,
        };
        assert_eq!(
            doors.interact(&w, &wall_target),
            Err(InteractError::NotADoor(ObjectId::from_u128(404)))
        );
        assert!(!doors.is_open(DOOR));
    }

    #[test]
    fn locks_and_access_labels_are_enforced() {
        let w = world();
        let mut doors = DoorRegistry::new();
        doors.sync(&w);
        doors.get_mut(DOOR).unwrap().required_access = Some("keycard.blue".into());
        let mut access = BTreeSet::new();
        assert_eq!(
            doors.interact(&w, &req(near(), &access)),
            Err(InteractError::AccessDenied("keycard.blue".into()))
        );
        access.insert("keycard.blue".to_string());
        assert_eq!(
            doors.interact(&w, &req(near(), &access)),
            Ok(DoorEvent::Opened(DOOR))
        );
        doors.get_mut(DOOR).unwrap().locked = true;
        assert_eq!(
            doors.interact(&w, &req(near(), &access)),
            Err(InteractError::Locked)
        );
    }

    #[test]
    fn sync_keeps_state_and_drops_removed_doors() {
        let mut w = world();
        let mut doors = DoorRegistry::new();
        doors.sync(&w);
        doors.get_mut(DOOR).unwrap().open = true;
        doors.sync(&w);
        assert!(
            doors.is_open(DOOR),
            "повторная синхронизация не сбрасывает состояние"
        );
        w.apply(WorldCommand::RemoveElement { id: DOOR }).unwrap();
        doors.sync(&w);
        assert!(doors.get(DOOR).is_none());
    }
}
