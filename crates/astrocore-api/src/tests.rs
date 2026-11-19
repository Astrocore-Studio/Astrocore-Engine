use astrocore_core::{EntityRegistry, ObjectId, PrototypeId};
use astrocore_math::glam::Vec3;
use astrocore_world::{FloorDef, FloorId, World, WorldCommand, WorldConfig};

use super::*;

fn drive(api: &mut dyn WorldApi) -> Revision {
    api.submit(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .unwrap();
    api.revision()
}

#[test]
fn world_api_is_object_safe_and_applies_commands() {
    let mut w = World::new("api", WorldConfig::default()).unwrap();
    assert_eq!(drive(&mut w), Revision(1));
    assert!(
        w.submit(WorldCommand::RemoveElement {
            id: ObjectId::from_u128(1)
        })
        .is_err()
    );
}

#[test]
fn entity_api_works_through_the_trait() {
    fn run(api: &mut dyn EntityApi) {
        let o = ObjectId::from_u128(5);
        let e = api.spawn_entity(o).unwrap();
        assert!(api.is_alive(e));
        assert_eq!(api.entity_of(o), Some(e));
        api.despawn_entity(e).unwrap();
        assert!(!api.is_alive(e));
    }
    run(&mut EntityRegistry::new());
}

#[test]
fn physics_query_exposes_masked_queries() {
    let w = World::new("p", WorldConfig::default()).unwrap();
    let bw = BoxWorld::from_world(&w);
    let q: &dyn PhysicsQuery = &bw;
    assert!(
        q.raycast_masked(Vec3::ZERO, Vec3::X, 10.0, u32::MAX)
            .is_none()
    );
    let shape = Aabb::new(Vec3::ZERO, Vec3::ONE);
    assert!(q.overlap_masked(&shape, u32::MAX).is_empty());
    assert!(q.shapecast(&shape, Vec3::X, u32::MAX).is_none());
    assert!(q.contacts(&shape, u32::MAX).is_empty());
}

#[test]
fn asset_registry_reports_licenses() {
    let m = AssetManifest::new();
    let r: &dyn AssetRegistry = &m;
    assert!(r.assets().is_empty() && r.unlicensed().is_empty());
    assert!(r.asset(&PrototypeId::new("t:x").unwrap()).is_none());
}

struct Counter;

impl EditorExtension for Counter {
    fn id(&self) -> &str {
        "test.counter"
    }

    fn inspector_rows(&self, world: &World, _o: ObjectId) -> Vec<(String, String)> {
        vec![("элементов".into(), world.element_count().to_string())]
    }
}

#[test]
fn editor_extensions_have_default_empty_actions() {
    let w = World::new("e", WorldConfig::default()).unwrap();
    let ext: Box<dyn EditorExtension> = Box::new(Counter);
    assert_eq!(ext.id(), "test.counter");
    assert_eq!(ext.inspector_rows(&w, ObjectId::from_u128(1))[0].1, "0");
    assert!(ext.actions(&w, &Default::default()).is_empty());
}

#[test]
fn simulation_topology_reports_rooms_and_changes() {
    let mut w = World::new("t", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .unwrap();
    let a = Topology::build(&w, FloorId(0)).unwrap();
    let b = Topology::build(&w, FloorId(0)).unwrap();
    assert!(b.changes_from(&a).is_empty());
    let _ = SimulationTopology::room_at(&a, astrocore_math::GridCell::new(0, 0));
}

#[test]
fn replication_registry_validates_names_limits_and_sizes() {
    let kind = |name: &str, max| ReplicatedKind {
        name: name.into(),
        channel: ReplicationChannel::UnreliableSequenced,
        interest: Interest::Radius(40.0),
        max_bytes: max,
    };
    let mut r = ReplicationRegistry::new();
    r.register(kind("game.health", 64)).unwrap();
    assert_eq!(
        r.register(kind("game.health", 64)),
        Err(ReplicationError::Duplicate("game.health".into()))
    );
    assert!(matches!(
        r.register(kind("Плохо", 64)),
        Err(ReplicationError::BadName(_))
    ));
    assert!(matches!(
        r.register(kind("a", 0)),
        Err(ReplicationError::BadLimit(0))
    ));
    assert!(matches!(
        r.register(kind("b", 1 << 20)),
        Err(ReplicationError::BadLimit(_))
    ));
    assert!(r.check("game.health", 64).is_ok());
    assert!(matches!(
        r.check("game.health", 65),
        Err(ReplicationError::TooLarge { .. })
    ));
    assert!(matches!(
        r.check("nope", 1),
        Err(ReplicationError::Unknown(_))
    ));
    assert_eq!(r.kinds().count(), 1);
}
