use astrocore_data::DataFormat;
use astrocore_ecs::{ComponentRegistry, EventWorldExt, GameWorldExt, LifecycleWorldExt};
use astrocore_map::{
    ContainerKind, ContainerWorldExt, EntityCoordinates, MapWorldExt, SpawnWorldExt, Tile,
    TileCoord, Transform, TransformWorldExt,
};
use astrocore_math::{Box2, Vec2};
use astrocore_net::NetServerPlugin;
use astrocore_prototypes::{LoadReport, Prototypes};
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

use super::*;

const PROTOTYPES: &str = r#"
- type: entity
  id: Wall
  components:
  - type: Transform
    anchored: true
  - type: Physics
    bodyType: Static
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeAabb
          bounds: "-0.5,-0.5,0.5,0.5"
        mask: [ FullTileMask ]
        layer: [ WallLayer ]
        density: 1000

- type: entity
  id: Mob
  components:
  - type: Physics
    bodyType: KinematicController
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeCircle
          radius: 0.35
        density: 185
        mask: [ MobMask ]
        layer: [ MobLayer ]

- type: entity
  id: Crate
  components:
  - type: Physics
    bodyType: Dynamic
    linearDamping: 0
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeAabb
          bounds: "-0.4,-0.4,0.4,0.4"
        density: 50
        mask: [ MachineMask ]
        layer: [ MachineLayer ]

- type: entity
  id: Ball
  components:
  - type: Physics
    bodyType: Dynamic
    linearDamping: 0
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeCircle
          radius: 0.2
        density: 10
        restitution: 1
        mask: [ ItemMask ]
        layer: 0

- type: entity
  id: Puddle
  components:
  - type: Transform
    anchored: true
  - type: Physics
    bodyType: Static
  - type: Fixtures
    fixtures:
      slip:
        shape: !type:PhysShapeAabb {}
        hard: false
        layer: [ SlipLayer ]
"#;

fn register_groups() {
    const OPAQUE: i32 = 1;
    const IMPASSABLE: i32 = 2;
    const MID: i32 = 4;
    const HIGH: i32 = 8;
    const LOW: i32 = 16;
    const BULLET: i32 = 64;
    const INTERACT: i32 = 128;
    for (name, bits) in [
        ("MobMask", IMPASSABLE | HIGH | MID | LOW),
        ("MobLayer", OPAQUE | BULLET),
        ("MachineMask", IMPASSABLE | MID | LOW),
        ("MachineLayer", OPAQUE | MID | LOW | BULLET),
        ("ItemMask", IMPASSABLE | HIGH),
        (
            "WallLayer",
            OPAQUE | IMPASSABLE | HIGH | MID | LOW | BULLET | INTERACT,
        ),
        ("FullTileMask", IMPASSABLE | HIGH | MID | LOW | INTERACT),
        ("SlipLayer", MID | LOW),
    ] {
        register_collision_group(name, bits);
    }
}

#[derive(Resource, Default)]
struct Log(Vec<String>);

struct Scene {
    world: World,
    grid: Entity,
}

impl Scene {
    fn new() -> Self {
        register_groups();
        let mut world = World::new();
        world.add_plugin(NetServerPlugin);
        world.add_plugin(PhysicsPlugin::SERVER);
        world.init_resource::<Log>();
        let mut prototypes = Prototypes::default();
        let mut report = LoadReport::default();
        prototypes.load_str("test.yml", DataFormat::Yaml, PROTOTYPES, &mut report);
        prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
        assert_eq!(report.errors, []);
        assert!(report.unknown_components.is_empty());
        world.insert_resource(prototypes);
        let (map, _) = world.create_map(true);
        let grid = world.create_grid(map);
        let floor =
            (0..20).flat_map(|x| (0..10).map(move |y| (TileCoord::new(x, y, 0), Tile::new(1))));
        world.set_tiles(grid, floor);
        Self { world, grid }
    }

    fn spawn(&mut self, prototype: &str, x: f32, y: f32) -> Entity {
        self.world
            .spawn_attached(
                prototype,
                EntityCoordinates::new(self.grid, Vec2::new(x, y)),
            )
            .unwrap()
    }

    fn position(&self, entity: Entity) -> Vec2 {
        self.world
            .get::<Transform>(entity)
            .unwrap()
            .local_position()
    }

    fn run(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.world.run_tick();
        }
    }

    /// Такты, в которых управление держит скорость персонажа.
    fn walk(&mut self, mob: Entity, velocity: Vec2, ticks: u32) {
        for _ in 0..ticks {
            self.world.set_linear_velocity(mob, velocity);
            self.world.run_tick();
        }
    }
}

#[test]
fn data_reads_shapes_groups_and_mass() {
    let scene = &mut Scene::new();
    let wall = scene.spawn("Wall", 5.5, 5.5);
    let mob = scene.spawn("Mob", 2.5, 2.5);
    let body = scene.world.get::<Physics>(wall).unwrap();
    assert_eq!(body.body_type, BodyType::Static);
    assert_eq!(body.inv_mass(), 0.0);
    let fixtures = scene.world.get::<Fixtures>(wall).unwrap();
    assert_eq!(
        fixtures.fixtures["fix1"].layer,
        CollisionGroups(1 | 2 | 8 | 4 | 16 | 64 | 128)
    );
    let mob_body = scene.world.get::<Physics>(mob).unwrap();
    let expected = 185.0 * std::f32::consts::PI * 0.35 * 0.35;
    assert!((mob_body.mass - expected).abs() < 1e-3);
    assert!(mob_body.inv_mass() > 0.0);
    // Мобы друг с другом по слоям не сталкиваются, со стеной — сталкиваются.
    let mob_fixture = &scene.world.get::<Fixtures>(mob).unwrap().fixtures["fix1"];
    assert!(!mob_fixture.should_collide(mob_fixture));
    assert!(mob_fixture.should_collide(&fixtures.fixtures["fix1"]));
}

#[test]
fn mob_is_stopped_by_wall_and_slides_along_it() {
    let scene = &mut Scene::new();
    for y in 0..10 {
        scene.spawn("Wall", 6.5, y as f32 + 0.5);
    }
    let mob = scene.spawn("Mob", 3.5, 5.5);
    scene.walk(mob, Vec2::new(4.0, 0.0), 60);
    let position = scene.position(mob);
    // Край стены на x = 6, радиус моба 0,35.
    assert!((position.x - 5.65).abs() < 0.02, "{position}");
    assert!((position.y - 5.5).abs() < 1e-3);

    // Движение по диагонали в стену: вдоль стены моб скользит.
    scene.walk(mob, Vec2::new(4.0, 3.0), 30);
    let position = scene.position(mob);
    assert!((position.x - 5.65).abs() < 0.02, "{position}");
    assert!((position.y - 8.5).abs() < 0.05, "{position}");
}

#[test]
fn fast_body_does_not_tunnel_through_wall() {
    let scene = &mut Scene::new();
    for y in 0..10 {
        scene.spawn("Wall", 6.5, y as f32 + 0.5);
    }
    let ball = scene.spawn("Ball", 2.5, 5.5);
    scene.world.set_linear_velocity(ball, Vec2::new(40.0, 0.0));
    scene.run(10);
    let position = scene.position(ball);
    assert!(position.x < 6.0, "{position}");
    // Упругий мяч отскочил назад.
    assert!(scene.world.get::<Physics>(ball).unwrap().linear_velocity.x < -30.0);
}

#[test]
fn mob_pushes_crate_and_crate_falls_asleep() {
    let scene = &mut Scene::new();
    let mob = scene.spawn("Mob", 2.5, 5.5);
    let crate_ = scene.spawn("Crate", 3.5, 5.5);
    assert!(!scene.world.get::<Physics>(crate_).unwrap().awake);
    scene.walk(mob, Vec2::new(3.0, 0.0), 30);
    let pushed = scene.position(crate_);
    assert!(pushed.x > 4.0, "{pushed}");
    assert!(scene.position(mob).x < pushed.x - 0.7);

    // Моб остановился: ящик без затухания едет дальше, с затуханием — засыпает.
    scene
        .world
        .get_mut::<Physics>(crate_)
        .unwrap()
        .linear_damping = 8.0;
    scene.world.set_linear_velocity(mob, Vec2::ZERO);
    scene.run(90);
    let body = scene.world.get::<Physics>(crate_).unwrap();
    assert!(
        !body.awake && body.linear_velocity == Vec2::ZERO,
        "{body:?}"
    );
}

#[test]
fn sensors_report_touch_without_blocking() {
    let scene = &mut Scene::new();
    scene
        .world
        .subscribe::<Fixtures, StartCollide>(|world, _, event: &mut StartCollide| {
            world
                .resource_mut::<Log>()
                .0
                .push(format!("start {}", event.our_fixture));
        });
    scene
        .world
        .subscribe::<Fixtures, EndCollide>(|world, _, event: &mut EndCollide| {
            world
                .resource_mut::<Log>()
                .0
                .push(format!("end {}", event.our_fixture));
        });
    scene.spawn("Puddle", 5.5, 5.5);
    let mob = scene.spawn("Mob", 3.5, 5.5);
    scene.walk(mob, Vec2::new(4.0, 0.0), 45);
    assert!(scene.position(mob).x > 8.0);
    let log = std::mem::take(&mut scene.world.resource_mut::<Log>().0);
    assert_eq!(log, ["start fix1", "start slip", "end fix1", "end slip"]);
    assert_eq!(scene.world.resource::<Broadphase>().contact_count(), 0);
}

#[test]
fn prevent_collide_lets_bodies_pass() {
    let scene = &mut Scene::new();
    for y in 0..10 {
        scene.spawn("Wall", 6.5, y as f32 + 0.5);
    }
    scene
        .world
        .subscribe::<Physics, PreventCollide>(|_, _, event: &mut PreventCollide| {
            event.cancelled = true;
        });
    let mob = scene.spawn("Mob", 4.5, 5.5);
    scene.walk(mob, Vec2::new(4.0, 0.0), 45);
    assert!(scene.position(mob).x > 8.0);
}

#[test]
fn anchoring_containers_and_deletion_update_broadphase() {
    let scene = &mut Scene::new();
    let crate_ = scene.spawn("Crate", 5.5, 5.5);
    let around = Box2::new(5.0, 5.0, 6.0, 6.0);
    let hits = |scene: &Scene| scene.world.entities_intersecting(scene.grid, 0, around);
    assert_eq!(hits(scene), [crate_]);
    assert!(
        scene
            .world
            .entities_intersecting(scene.grid, 1, around)
            .is_empty()
    );

    // Привязка делает тело неподвижным, отвязка — снова подвижным.
    scene.world.anchor(crate_);
    assert_eq!(
        scene.world.get::<Physics>(crate_).unwrap().body_type,
        BodyType::Static
    );
    scene.world.unanchor(crate_);
    assert_eq!(
        scene.world.get::<Physics>(crate_).unwrap().body_type,
        BodyType::Dynamic
    );

    // В контейнере тело в столкновениях не участвует.
    let locker = scene.spawn("Wall", 1.5, 1.5);
    scene
        .world
        .ensure_container(locker, "storage", ContainerKind::List);
    scene
        .world
        .insert_into_container(crate_, locker, "storage")
        .unwrap();
    assert!(hits(scene).is_empty());
    scene.world.remove_from_container(crate_, None).unwrap();
    scene
        .world
        .set_local_position(crate_, Vec2::new(5.5, 5.5))
        .unwrap();
    assert_eq!(hits(scene), [crate_]);

    scene.world.set_can_collide(crate_, false);
    assert!(hits(scene).is_empty());
    scene.world.set_can_collide(crate_, true);
    scene.world.delete_entity(crate_);
    assert!(hits(scene).is_empty());
}

#[test]
fn single_body_step_matches_full_step() {
    // Предсказание на клиенте считает одно тело: результат совпадает с общим шагом.
    let mut full = Scene::new();
    let mut solo = Scene::new();
    let mut mobs = Vec::new();
    for scene in [&mut full, &mut solo] {
        for y in 0..10 {
            scene.spawn("Wall", 6.5, y as f32 + 0.5);
        }
        mobs.push(scene.spawn("Mob", 4.5, 5.5));
    }
    for _ in 0..40 {
        full.world.set_linear_velocity(mobs[0], Vec2::new(3.0, 1.0));
        full.world.run_tick();
        solo.world.set_linear_velocity(mobs[1], Vec2::new(3.0, 1.0));
        step_physics(&mut solo.world, 1.0 / 30.0, Some(mobs[1]));
    }
    assert_eq!(full.position(mobs[0]), solo.position(mobs[1]));
}

#[test]
fn rays_stop_at_hard_fixtures_of_the_mask_on_grids_and_in_space() {
    let mut scene = Scene::new();
    let wall = scene.spawn("Wall", 5.5, 2.5);
    let puddle = scene.spawn("Puddle", 3.5, 2.5);
    let mob = scene.spawn("Mob", 1.5, 2.5);
    let ball = scene.spawn("Ball", 8.5, 2.5);
    scene.run(1);
    let map = scene.world.map_coordinates(mob).unwrap();
    let mask = CollisionGroups(2 | 128); // Impassable | InteractImpassable
    let ray = |direction: Vec2, max: f32| Ray {
        origin: map,
        direction,
        max_distance: max,
        mask,
    };
    let none = |_: Entity| false;

    // Нетвёрдая лужа и сам игрок, внутри которого начинается луч, не мешают; стена — да.
    let hits = scene.world.intersect_ray(ray(Vec2::X, 10.0), &none, false);
    assert_eq!(hits.iter().map(|h| h.entity).collect::<Vec<_>>(), [wall]);
    assert!((hits[0].distance - 3.5).abs() < 1e-4, "{hits:?}");
    assert!((hits[0].point.x - 5.0).abs() < 1e-4);
    assert!(!hits.iter().any(|h| h.entity == puddle || h.entity == ball));
    // Короткий луч не достаёт, другая маска стену не видит, предикат её пропускает.
    assert!(
        scene
            .world
            .intersect_ray(ray(Vec2::X, 3.0), &none, false)
            .is_empty()
    );
    let other_mask = Ray {
        mask: CollisionGroups(1 << 20),
        ..ray(Vec2::X, 10.0)
    };
    assert!(
        scene
            .world
            .intersect_ray(other_mask, &none, false)
            .is_empty()
    );
    let skip_wall = |entity: Entity| entity == wall;
    assert!(
        scene
            .world
            .intersect_ray(ray(Vec2::X, 10.0), &skip_wall, false)
            .is_empty()
    );
    assert_eq!(
        scene.world.unobstructed_distance(ray(Vec2::X, 10.0), &none),
        hits[0].distance
    );
    assert_eq!(
        scene.world.unobstructed_distance(ray(Vec2::Y, 2.0), &none),
        2.0
    );

    // Повёрнутый и сдвинутый грид: луч переводится в его оси.
    let map_entity = scene
        .world
        .get::<Transform>(scene.grid)
        .unwrap()
        .map()
        .unwrap();
    let shuttle = scene.world.create_grid(map_entity);
    scene
        .world
        .set_coordinates(
            shuttle,
            EntityCoordinates::new(map_entity, Vec2::new(30.0, 0.0)),
            Some(astrocore_math::Angle(std::f32::consts::FRAC_PI_2)),
        )
        .unwrap();
    scene
        .world
        .set_tile(shuttle, TileCoord::new(0, 0, 0), Tile::new(1));
    let shuttle_wall = scene
        .world
        .spawn_attached("Wall", EntityCoordinates::new(shuttle, Vec2::new(0.5, 0.5)))
        .unwrap();
    // Центр стены шаттла на карте: (30 - 0.5, 0.5).
    let from_space = Ray {
        origin: astrocore_map::MapCoordinates::new(map.map, Vec2::new(25.0, 0.5)),
        ..ray(Vec2::X, 10.0)
    };
    let hits = scene.world.intersect_ray(from_space, &none, true);
    assert_eq!(
        hits.iter().map(|h| h.entity).collect::<Vec<_>>(),
        [shuttle_wall]
    );
    assert!((hits[0].distance - 4.0).abs() < 1e-4, "{hits:?}");
}

#[test]
fn friction_slows_sliding_and_off_centre_hits_spin_free_bodies() {
    let mut scene = Scene::new();
    // Ящик скользит вдоль стены, прижимаясь к ней: трение гасит скорость вдоль стены.
    for x in 0..20 {
        scene.spawn("Wall", x as f32 + 0.5, 0.5);
    }
    let sliding = scene.spawn("Crate", 2.5, 1.45);
    scene.run(1);
    scene
        .world
        .set_linear_velocity(sliding, Vec2::new(4.0, -2.0));
    let mut free = Scene::new();
    for x in 0..20 {
        free.spawn("Wall", x as f32 + 0.5, 0.5);
    }
    let reference = free.spawn("Crate", 2.5, 1.45);
    free.run(1);
    for fixtures in free
        .world
        .get_mut::<Fixtures>(reference)
        .unwrap()
        .fixtures
        .values_mut()
    {
        fixtures.friction = 0.0;
    }
    free.world
        .set_linear_velocity(reference, Vec2::new(4.0, -2.0));
    scene.run(10);
    free.run(10);
    let with_friction = scene.position(sliding).x;
    let without = free.position(reference).x;
    assert!(
        with_friction < without - 0.05,
        "трение {with_friction}, без трения {without}"
    );

    // Тело, попавшее в край свободно вращающегося ящика, раскручивает его.
    let mut scene = Scene::new();
    let target = scene.spawn("Crate", 5.5, 5.5);
    scene.run(1);
    scene
        .world
        .get_mut::<Physics>(target)
        .unwrap()
        .fixed_rotation = false;
    assert!(scene.world.get::<Physics>(target).unwrap().inv_inertia() > 0.0);
    // Второй ящик летит выше центра цели: слои ящиков сталкиваются друг с другом.
    let ball = scene.spawn("Crate", 3.0, 6.1);
    scene.run(1);
    scene.world.set_linear_velocity(ball, Vec2::new(6.0, 0.0));
    scene.run(15);
    let spin = scene.world.get::<Physics>(target).unwrap().angular_velocity;
    assert!(spin.abs() > 0.05, "угловая скорость {spin}");
    // Удар выше центра слева направо закручивает по часовой стрелке.
    assert!(spin < 0.0, "угловая скорость {spin}");

    // Вращающий импульс: тела с фиксированным поворотом не раскручиваются.
    scene.world.apply_angular_impulse(ball, 1.0);
    let wall = scene.spawn("Wall", 9.5, 9.5);
    scene.world.apply_angular_impulse(wall, 1.0);
    assert_eq!(
        scene.world.get::<Physics>(wall).unwrap().angular_velocity,
        0.0
    );
}

#[test]
fn inertia_of_shapes() {
    use crate::shapes::Shape;
    let unit = Shape::PhysShapeAabb {
        bounds: Box2::new(-0.5, -0.5, 0.5, 0.5),
    };
    assert!((unit.unit_inertia() - 1.0 / 6.0).abs() < 1e-6);
    let polygon = Shape::PolygonShape {
        vertices: vec![
            Vec2::new(-0.5, -0.5),
            Vec2::new(0.5, -0.5),
            Vec2::new(0.5, 0.5),
            Vec2::new(-0.5, 0.5),
        ],
    };
    assert!((polygon.unit_inertia() - 1.0 / 6.0).abs() < 1e-6);
    let circle = Shape::PhysShapeCircle {
        radius: 1.0,
        position: Vec2::ZERO,
    };
    assert!((circle.unit_inertia() - std::f32::consts::PI / 2.0).abs() < 1e-5);
}

#[test]
fn bodies_report_falling_asleep_and_runtime_fixtures_count() {
    let mut scene = Scene::new();
    let crate_ = scene.spawn("Crate", 5.5, 5.5);
    scene
        .world
        .subscribe::<Physics, PhysicsSleep>(|world, entity, _| {
            world
                .resource_mut::<Log>()
                .0
                .push(format!("sleep {entity:?}"));
        });
    scene.world.set_linear_velocity(crate_, Vec2::new(0.5, 0.0));
    scene
        .world
        .get_mut::<Physics>(crate_)
        .unwrap()
        .linear_damping = 20.0;
    scene.run(40);
    assert_eq!(
        scene.world.resource::<Log>().0,
        [format!("sleep {crate_:?}")]
    );

    // Фикстура, добавленная во время игры, учитывается после пересчёта.
    let mass = scene.world.get::<Physics>(crate_).unwrap().mass;
    scene
        .world
        .get_mut::<Fixtures>(crate_)
        .unwrap()
        .fixtures
        .insert("extra".into(), Fixture::default());
    scene.world.refresh_fixtures(crate_);
    assert!(scene.world.get::<Physics>(crate_).unwrap().mass > mass);
}

#[test]
fn distance_joint_drags_a_free_body_like_a_rope() {
    let scene = &mut Scene::new();
    let mob = scene.spawn("Mob", 3.5, 5.5);
    let crate_ = scene.spawn("Crate", 2.4, 5.5);
    scene.run(1);
    assert!(!scene.world.get::<Physics>(crate_).unwrap().awake);
    let joint = scene
        .world
        .create_distance_joint(crate_, mob, Vec2::ZERO, Vec2::ZERO, "pull")
        .unwrap();
    assert!((joint.distance().unwrap().length - 1.1).abs() < 1e-4);
    scene.world.edit_joint(crate_, "pull", |joint| {
        joint.collide_connected = false;
        let rope = joint.distance_mut().unwrap();
        rope.max_length = rope.length + 0.15;
        rope.min_length = 0.0;
    });
    assert!(scene.world.joint(mob, "pull").is_some());

    // Моб уходит вправо: ящик тянется следом не дальше длины верёвки, моба не тормозит.
    scene.walk(mob, Vec2::new(3.0, 0.0), 45);
    let (mob_at, crate_at) = (scene.position(mob), scene.position(crate_));
    assert!(mob_at.x > 7.9, "{mob_at}");
    assert!(crate_at.x > 6.6, "{crate_at}");
    assert!(
        mob_at.distance(crate_at) <= 1.25 + 0.05,
        "{}",
        mob_at.distance(crate_at)
    );

    // Моб идёт назад сквозь ящик: соединённые тела не сталкиваются, верёвка не толкает.
    scene.walk(mob, Vec2::new(-3.0, 0.0), 20);
    assert!(scene.position(mob).x < scene.position(crate_).x);

    // Удалённое тело отпускает сочленение у второго.
    scene.world.delete_entity(mob);
    assert!(scene.world.joint(crate_, "pull").is_none());
}

#[test]
fn paused_bodies_stand_still_unless_they_ignore_pause() {
    use astrocore_ecs::LifecycleWorldExt;

    let scene = &mut Scene::new();
    let still = scene.spawn("Mob", 2.5, 2.5);
    let ghost = scene.spawn("Mob", 2.5, 6.5);
    scene.world.set_entity_paused(still, true);
    scene.world.set_entity_paused(ghost, true);
    scene.world.get_mut::<Physics>(ghost).unwrap().ignore_paused = true;
    scene.walk(still, Vec2::new(4.0, 0.0), 10);
    scene.walk(ghost, Vec2::new(4.0, 0.0), 10);
    assert_eq!(scene.position(still).x, 2.5);
    assert!(scene.position(ghost).x > 3.5, "{}", scene.position(ghost));
}
