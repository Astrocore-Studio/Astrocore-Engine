//! Инфраструктура подключаемых симуляций (раздел 14.3 дизайн-документа).
//!
//! Движок даёт общие примитивы, а **что именно** считается (газы, электричество, магия)
//! решает билд:
//! * [`SimulationModule`] — модуль, который получает изменения топологии и тикает;
//! * [`ConnectionGraph`] — граф соединений с событиями (сети питания, трубы, сигналы);
//! * [`SimulationSet`] — расписание модулей с проверкой зависимостей; сервер **не тратит
//!   вычисления** на неподключённые системы.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use astrocore_core::ObjectId;
use astrocore_spatial::{Topology, TopologyChange};

use crate::streaming::{Activity, ReducedBehavior};

/// Событие изменения графа соединений.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphEvent {
    NodeAdded(ObjectId),
    NodeRemoved(ObjectId),
    Connected(ObjectId, ObjectId),
    Disconnected(ObjectId, ObjectId),
    /// Состав связной компоненты изменился (идентификатор — наименьший узел).
    ComponentChanged(ObjectId),
}

/// Неориентированный граф соединений между объектами.
#[derive(Debug, Default, Clone)]
pub struct ConnectionGraph {
    edges: BTreeMap<ObjectId, BTreeSet<ObjectId>>,
    events: Vec<GraphEvent>,
}

impl ConnectionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, id: ObjectId) {
        if self.edges.insert(id, BTreeSet::new()).is_none() {
            self.events.push(GraphEvent::NodeAdded(id));
        }
    }

    pub fn remove_node(&mut self, id: ObjectId) {
        let Some(neighbors) = self.edges.remove(&id) else {
            return;
        };
        for n in &neighbors {
            if let Some(set) = self.edges.get_mut(n) {
                set.remove(&id);
            }
            self.events
                .push(GraphEvent::Disconnected(id.min(*n), id.max(*n)));
        }
        self.events.push(GraphEvent::NodeRemoved(id));
        for n in neighbors {
            self.events.push(GraphEvent::ComponentChanged(
                self.component_of(n).into_iter().next().unwrap_or(n),
            ));
        }
    }

    /// Соединить узлы (оба должны существовать). `false` — соединение уже было или узла нет.
    pub fn connect(&mut self, a: ObjectId, b: ObjectId) -> bool {
        if a == b || !self.edges.contains_key(&a) || !self.edges.contains_key(&b) {
            return false;
        }
        let added = self.edges.get_mut(&a).is_some_and(|s| s.insert(b))
            & self.edges.get_mut(&b).is_some_and(|s| s.insert(a));
        if added {
            self.events.push(GraphEvent::Connected(a.min(b), a.max(b)));
            self.events.push(GraphEvent::ComponentChanged(
                self.component_of(a).into_iter().next().unwrap_or(a),
            ));
        }
        added
    }

    pub fn disconnect(&mut self, a: ObjectId, b: ObjectId) -> bool {
        let removed = self.edges.get_mut(&a).is_some_and(|s| s.remove(&b))
            & self.edges.get_mut(&b).is_some_and(|s| s.remove(&a));
        if removed {
            self.events
                .push(GraphEvent::Disconnected(a.min(b), a.max(b)));
            for n in [a, b] {
                self.events.push(GraphEvent::ComponentChanged(
                    self.component_of(n).into_iter().next().unwrap_or(n),
                ));
            }
        }
        removed
    }

    /// Связная компонента узла (по возрастанию `ObjectId`).
    pub fn component_of(&self, start: ObjectId) -> BTreeSet<ObjectId> {
        let mut seen = BTreeSet::new();
        if !self.edges.contains_key(&start) {
            return seen;
        }
        let mut queue = VecDeque::from([start]);
        seen.insert(start);
        while let Some(n) = queue.pop_front() {
            for m in self.edges.get(&n).into_iter().flatten() {
                if seen.insert(*m) {
                    queue.push_back(*m);
                }
            }
        }
        seen
    }

    pub fn is_connected(&self, a: ObjectId, b: ObjectId) -> bool {
        self.component_of(a).contains(&b)
    }

    pub fn node_count(&self) -> usize {
        self.edges.len()
    }

    /// Забрать накопленные события.
    pub fn take_events(&mut self) -> Vec<GraphEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Подключаемая симуляция.
pub trait SimulationModule {
    fn id(&self) -> &str;

    fn depends_on(&self) -> &[&'static str] {
        &[]
    }

    /// Что модуль умеет при пониженной активности (см. `streaming`).
    fn reduced_behavior(&self) -> ReducedBehavior {
        ReducedBehavior::NotAllowed
    }

    /// Топология этажа изменилась.
    fn on_topology_changed(&mut self, _topology: &Topology, _changes: &[TopologyChange]) {}

    /// Один тик при заданном уровне активности (0 — пропущен, если модуль не подключён).
    fn tick(&mut self, dt: f32, activity: Activity);
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimError {
    #[error("модуль {0:?} уже подключён")]
    Duplicate(String),
    #[error("модуль {module:?} зависит от неподключённого {missing:?}")]
    MissingDependency { module: String, missing: String },
    #[error("циклическая зависимость модулей: {0}")]
    Cycle(String),
}

/// Набор подключённых симуляций. Пустой набор не стоит ничего.
#[derive(Default)]
pub struct SimulationSet {
    modules: Vec<Box<dyn SimulationModule>>,
    order: Vec<usize>,
}

impl SimulationSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, module: Box<dyn SimulationModule>) -> Result<(), SimError> {
        if self.modules.iter().any(|m| m.id() == module.id()) {
            return Err(SimError::Duplicate(module.id().to_string()));
        }
        self.modules.push(module);
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<(), SimError> {
        let ids: BTreeMap<&str, usize> = self
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id(), i))
            .collect();
        for m in &self.modules {
            for d in m.depends_on() {
                if !ids.contains_key(d) {
                    return Err(SimError::MissingDependency {
                        module: m.id().into(),
                        missing: (*d).into(),
                    });
                }
            }
        }
        let (mut order, mut done) = (Vec::new(), BTreeSet::new());
        while order.len() < self.modules.len() {
            let before = order.len();
            for (i, m) in self.modules.iter().enumerate() {
                if !done.contains(&i) && m.depends_on().iter().all(|d| done.contains(&ids[d])) {
                    done.insert(i);
                    order.push(i);
                }
            }
            if order.len() == before {
                let stuck: Vec<&str> = self
                    .modules
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !done.contains(i))
                    .map(|(_, m)| m.id())
                    .collect();
                return Err(SimError::Cycle(stuck.join(", ")));
            }
        }
        self.order = order;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn topology_changed(&mut self, topology: &Topology, changes: &[TopologyChange]) {
        for &i in &self.order {
            self.modules[i].on_topology_changed(topology, changes);
        }
    }

    pub fn tick(&mut self, dt: f32, activity: Activity) {
        for &i in &self.order {
            self.modules[i].tick(dt, activity);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use astrocore_core::PrototypeId;
    use astrocore_math::{GridCell, GridEdge};
    use astrocore_spatial::{Channel, diff};
    use astrocore_world::{
        FloorDef, FloorId, OpeningKind, Placement, StructureElement, StructureKind, World,
        WorldCommand, WorldConfig,
    };

    use super::*;

    fn n(v: u128) -> ObjectId {
        ObjectId::from_u128(v)
    }

    #[test]
    fn graph_components_and_events() {
        let mut g = ConnectionGraph::new();
        for i in 1..=5 {
            g.add_node(n(i));
        }
        assert!(g.connect(n(1), n(2)) && g.connect(n(2), n(3)) && g.connect(n(4), n(5)));
        assert!(!g.connect(n(1), n(2)), "повтор");
        assert!(!g.connect(n(1), n(1)) && !g.connect(n(1), n(99)));
        assert!(g.is_connected(n(1), n(3)) && !g.is_connected(n(1), n(4)));
        assert_eq!(g.component_of(n(2)), BTreeSet::from([n(1), n(2), n(3)]));
        g.take_events();
        assert!(g.disconnect(n(2), n(3)));
        assert!(!g.is_connected(n(1), n(3)));
        let ev = g.take_events();
        assert!(ev.contains(&GraphEvent::Disconnected(n(2), n(3))));
        assert!(
            ev.iter()
                .filter(|e| matches!(e, GraphEvent::ComponentChanged(_)))
                .count()
                == 2
        );
        g.remove_node(n(2));
        assert_eq!(g.node_count(), 4);
        assert!(g.take_events().contains(&GraphEvent::NodeRemoved(n(2))));
        assert!(g.component_of(n(2)).is_empty());
    }

    struct Log(
        Rc<RefCell<Vec<String>>>,
        &'static str,
        &'static [&'static str],
    );
    impl SimulationModule for Log {
        fn id(&self) -> &str {
            self.1
        }
        fn depends_on(&self) -> &[&'static str] {
            self.2
        }
        fn tick(&mut self, _dt: f32, a: Activity) {
            self.0.borrow_mut().push(format!("{}:{a:?}", self.1));
        }
    }

    #[test]
    fn modules_tick_in_dependency_order_and_empty_sets_cost_nothing() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut set = SimulationSet::new();
        assert!(set.is_empty());
        set.tick(0.05, Activity::Active); // без модулей ничего не происходит
        set.add(Box::new(Log(log.clone(), "fire", &["atmosphere"])))
            .unwrap();
        set.add(Box::new(Log(log.clone(), "atmosphere", &[])))
            .unwrap();
        set.finalize().unwrap();
        set.tick(0.05, Activity::Reduced);
        assert_eq!(*log.borrow(), vec!["atmosphere:Reduced", "fire:Reduced"]);
    }

    #[test]
    fn invalid_module_sets_are_rejected() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut set = SimulationSet::new();
        set.add(Box::new(Log(log.clone(), "a", &[]))).unwrap();
        assert!(matches!(
            set.add(Box::new(Log(log.clone(), "a", &[]))),
            Err(SimError::Duplicate(_))
        ));
        let mut missing = SimulationSet::new();
        missing
            .add(Box::new(Log(log.clone(), "x", &["ghost"])))
            .unwrap();
        assert!(matches!(
            missing.finalize(),
            Err(SimError::MissingDependency { .. })
        ));
        let mut cyc = SimulationSet::new();
        cyc.add(Box::new(Log(log.clone(), "p", &["q"]))).unwrap();
        cyc.add(Box::new(Log(log, "q", &["p"]))).unwrap();
        assert!(matches!(cyc.finalize(), Err(SimError::Cycle(_))));
    }

    /// Пример билда: «газообмен» строит граф из комнат и портальных каналов воздуха.
    /// Показывает, что общего API топологии достаточно для собственной физики без единой формулы в движке.
    struct Atmosphere {
        graph: ConnectionGraph,
        open_doors: BTreeSet<ObjectId>,
        rooms: usize,
    }

    impl SimulationModule for Atmosphere {
        fn id(&self) -> &str {
            "example.atmosphere"
        }
        fn on_topology_changed(&mut self, t: &Topology, _c: &[TopologyChange]) {
            self.graph = ConnectionGraph::new();
            self.rooms = t.rooms.len();
            for r in &t.rooms {
                self.graph.add_node(n(u128::from(r.id.0) + 1));
            }
            for p in &t.portals {
                let open = self.open_doors.contains(&p.element);
                if astrocore_spatial::PortalChannels::for_opening(p.kind, open).passes(Channel::Air)
                {
                    self.graph
                        .connect(n(u128::from(p.a.0) + 1), n(u128::from(p.b.0) + 1));
                }
            }
        }
        fn tick(&mut self, _dt: f32, _a: Activity) {}
    }

    #[test]
    fn a_game_module_can_build_gas_exchange_from_the_topology_api() {
        let mut w = World::new("t", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 0.0,
            height: 3.0,
        }))
        .unwrap();
        let mut id = 1;
        let mut put = |w: &mut World, kind, edge| {
            w.apply(WorldCommand::PlaceElement(StructureElement {
                id: n(id),
                kind,
                placement: Placement::Edge(edge),
                floor: FloorId(0),
                material: PrototypeId::new("t:m").unwrap(),
                thickness: 0.2,
            }))
            .unwrap();
            id += 1;
        };
        for i in 0..2 {
            for e in [
                GridEdge::along_x(i, 0),
                GridEdge::along_x(i, 2),
                GridEdge::along_z(0, i),
                GridEdge::along_z(2, i),
            ] {
                if e == GridEdge::along_x(0, 2) {
                    put(&mut w, StructureKind::Opening(OpeningKind::Door), e);
                } else {
                    put(&mut w, StructureKind::Wall, e);
                }
            }
        }
        let door = w
            .edge_element(FloorId(0), GridEdge::along_x(0, 2))
            .unwrap()
            .id;
        let before = Topology::build(&w, FloorId(0)).unwrap();
        let mut atm = Atmosphere {
            graph: ConnectionGraph::new(),
            open_doors: BTreeSet::new(),
            rooms: 0,
        };
        atm.on_topology_changed(&before, &[]);
        let (inside, outside) = (
            before.room_at(GridCell::new(0, 0)).unwrap(),
            before.room_at(GridCell::new(-1, -1)).unwrap(),
        );
        let node = |r: astrocore_spatial::RoomId| n(u128::from(r.0) + 1);
        assert!(
            !atm.graph.is_connected(node(inside), node(outside)),
            "закрытая дверь не пускает воздух"
        );
        atm.open_doors.insert(door);
        atm.on_topology_changed(&before, &[]);
        assert!(
            atm.graph.is_connected(node(inside), node(outside)),
            "открытая — пускает"
        );

        // Разрушение стены: топология меняется, модуль получает изменения и пересобирает граф.
        atm.open_doors.clear();
        let wall = w
            .edge_element(FloorId(0), GridEdge::along_z(2, 0))
            .unwrap()
            .id;
        w.apply(WorldCommand::RemoveElement { id: wall }).unwrap();
        let after = Topology::build(&w, FloorId(0)).unwrap();
        let changes = diff(&before, &after);
        assert!(!changes.is_empty());
        let mut set = SimulationSet::new();
        set.add(Box::new(atm)).unwrap();
        set.finalize().unwrap();
        set.topology_changed(&after, &changes);
    }
}
