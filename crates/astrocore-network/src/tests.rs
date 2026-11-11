use std::f32::consts::FRAC_PI_2;

use astrocore_core::ObjectId;
use astrocore_math::glam::Vec3;
use astrocore_math::{GridCell, GridEdge};
use astrocore_protocol::{
    ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage, decode, encode,
};
use astrocore_world::{FloorId, Placement, StructureElement, StructureKind, WorldCommand};

use crate::client::ClientEvent;
use crate::server::{ConnId, Out};
use crate::testkit::{DOOR_ID, Harness, material};

fn harness() -> Harness {
    Harness::new(Harness::default_cfg())
}

fn rejects(events: &[ClientEvent], reason: RejectReason) -> bool {
    events
        .iter()
        .any(|e| matches!(e, ClientEvent::Rejected { reason: r, .. } if *r == reason))
}

fn horizontal(v: Vec3) -> f32 {
    Vec3::new(v.x, 0.0, v.z).length()
}

/// Идти вперёд (−Z при yaw = 0) к двери и т. п.: yaw подбирается под направление.
const EAST: f32 = -FRAC_PI_2; // при yaw = −90° «вперёд» смотрит на +X
const WEST: f32 = FRAC_PI_2;

#[test]
fn handshake_delivers_world_doors_and_spawn() {
    let mut h = harness();
    let a = h.join("a");
    let c = h.client(a);
    assert!(c.is_ready());
    assert_eq!(
        c.world().unwrap().content_hash(),
        h.server.world().content_hash()
    );
    assert_eq!(c.position(), Vec3::new(2.5, 1.0, 2.5));
    assert_eq!(c.door_ids().len(), 1);
    assert_eq!(c.door_open(c.door_ids()[0]), Some(false));
    let ev = h.take_events(a);
    assert!(ev.iter().any(|e| matches!(e, ClientEvent::Joined { .. })));
    assert!(
        ev.iter()
            .any(|e| matches!(e, ClientEvent::SnapshotApplied { .. }))
    );
}

#[test]
fn incompatible_clients_are_refused() {
    let mut h = harness();
    // Другой мир.
    let bad_world = crate::client::ClientCore::new("x", "other-world", 0);
    let id = ConnId(100);
    h.server.connect(id);
    let outs = h.server.on_packet(id, &bad_world.hello_packet());
    assert!(outs.iter().any(|o| matches!(
        o,
        Out::Send {
            msg: ServerMessage::Reject {
                reason: RejectReason::IncompatibleWorld,
                ..
            },
            ..
        }
    )));
    assert!(outs.contains(&Out::Disconnect(id)));

    // Правильный мир, но устаревшая карта.
    let stale = crate::client::ClientCore::new("x", "testkit", 12345);
    let id = ConnId(101);
    h.server.connect(id);
    let outs = h.server.on_packet(id, &stale.hello_packet());
    assert!(outs.contains(&Out::Disconnect(id)));

    // Другая версия протокола.
    let mut packet = encode(&ClientMessage::RequestSnapshot).unwrap();
    packet[..2].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
    let id = ConnId(102);
    h.server.connect(id);
    let outs = h.server.on_packet(id, &packet);
    assert!(outs.iter().any(|o| matches!(
        o,
        Out::Send {
            msg: ServerMessage::Reject {
                reason: RejectReason::IncompatibleProtocol,
                ..
            },
            ..
        }
    )));
    assert!(outs.contains(&Out::Disconnect(id)));
    assert_eq!(h.server.player_count(), 0);
}

#[test]
fn messages_before_hello_and_garbage_are_refused() {
    let mut h = harness();
    let id = ConnId(7);
    h.server.connect(id);
    let outs = h
        .server
        .on_packet(id, &encode(&ClientMessage::RequestSnapshot).unwrap());
    assert!(outs.iter().any(|o| matches!(
        o,
        Out::Send {
            msg: ServerMessage::Reject {
                reason: RejectReason::NotAuthorized,
                ..
            },
            ..
        }
    )));
    assert!(
        outs.contains(&Out::Disconnect(id)),
        "не вошедший шлёт не Hello — отключаем"
    );
    let id = ConnId(8);
    h.server.connect(id);
    let outs = h.server.on_packet(id, &[1, 2, 3]);
    assert!(outs.contains(&Out::Disconnect(id)));
}

#[test]
fn silent_connections_time_out() {
    let mut cfg = Harness::default_cfg();
    cfg.handshake_timeout_ticks = 5;
    let mut h = Harness::new(cfg);
    let id = ConnId(1);
    h.server.connect(id);
    let mut dropped_at = None;
    for t in 1..=10 {
        if h.server.tick().contains(&Out::Disconnect(id)) {
            dropped_at = Some(t);
            break;
        }
    }
    assert_eq!(
        dropped_at,
        Some(6),
        "молчащее соединение закрывается после таймаута"
    );
}

#[test]
fn server_full_is_reported() {
    let mut cfg = Harness::default_cfg();
    cfg.max_clients = 1;
    let mut h = Harness::new(cfg);
    h.join("a");
    let outs = h.server.connect(ConnId(99));
    assert!(outs.iter().any(|o| matches!(
        o,
        Out::Send {
            msg: ServerMessage::Reject {
                reason: RejectReason::ServerFull,
                ..
            },
            ..
        }
    )));
}

#[test]
fn server_applies_one_input_per_tick_whatever_the_client_sends() {
    let mut h = harness();
    let a = h.join("a");
    h.tick();
    let start = h.server.player_position(a).unwrap();
    // «Читер» присылает 50 команд движения за один тик.
    for seq in 1..=50 {
        let p = encode(&ClientMessage::Input {
            seq,
            move_x: 0,
            move_z: 1,
            yaw: WEST,
        })
        .unwrap();
        h.send(a, p);
    }
    h.tick();
    let moved = horizontal(h.server.player_position(a).unwrap() - start);
    // Не больше одного тика движения: 4 м/с × 0.05 с = 0.2 м.
    assert!(moved <= 0.2 + 1e-3, "за тик пройдено {moved} м");
    assert!(moved > 0.1);
}

#[test]
fn invalid_input_values_do_not_move_players_beyond_limits() {
    let mut h = harness();
    let a = h.join("a");
    let before = h.server.player_position(a).unwrap();
    // Значения вне диапазона отвергаются на уровне протокола, поэтому «ускоренный» ввод невозможен.
    let mut raw = PROTOCOL_VERSION.to_le_bytes().to_vec();
    raw.extend(
        postcard::to_stdvec(&ClientMessage::Input {
            seq: 1,
            move_x: 100,
            move_z: 100,
            yaw: 0.0,
        })
        .unwrap(),
    );
    h.send(a, raw);
    h.ticks(3);
    assert!(horizontal(h.server.player_position(a).unwrap() - before) < 0.05);
}

#[test]
fn prediction_matches_the_server_without_corrections() {
    let mut h = harness();
    let a = h.join("a");
    h.take_events(a);
    for i in 0..40 {
        let yaw = if i < 20 { WEST } else { EAST };
        h.input(a, 0, 1, yaw);
    }
    h.ticks(2);
    let events = h.take_events(a);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ClientEvent::Corrected { .. })),
        "{events:?}"
    );
    let server = h.server.player_position(a).unwrap();
    assert!((h.client(a).position() - server).length() < 1e-3);
    assert_eq!(h.client(a).pending_inputs(), 0, "всё подтверждено");
}

#[test]
fn client_prediction_is_corrected_when_it_disagrees_with_the_server() {
    let mut h = harness();
    let a = h.join("a");
    // Сервер ставит стену на пути игрока (идёт на +X от x=2.5), но клиент не получает операцию:
    // его предсказание «проходит» сквозь стену, а сервер останавливает.
    h.drop_filter = Some(Box::new(|_, m| matches!(m, ServerMessage::WorldOp(_))));
    let wall = WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(777_001),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_z(4, 2)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.2,
    });
    let outs = h.server.apply_world_command(wall).unwrap();
    h.deliver(outs);
    h.take_events(a);
    let mut corrections = 0;
    let mut worst = 0.0f32;
    for _ in 0..30 {
        h.input(a, 0, 1, EAST);
        for e in h.take_events(a) {
            if let ClientEvent::Corrected { distance } = e {
                corrections += 1;
                worst = worst.max(distance);
            }
        }
    }
    h.ticks(3);
    assert!(
        corrections > 0,
        "клиент должен получить поправки от сервера"
    );
    assert!(worst > 0.1);
    let (server, client) = (h.server.player_position(a).unwrap(), h.client(a).position());
    assert!(server.x < 4.0, "сервер удержал игрока у стены: {server}");
    assert!(
        (client - server).length() < 0.01,
        "после согласования позиции совпадают: {client} vs {server}"
    );
}

#[test]
fn door_interaction_is_range_checked_and_updates_collision_for_everyone() {
    let mut h = harness();
    let a = h.join("a");
    let b = h.join("b");
    let door = h.client(a).door_ids()[0];

    // Слишком далеко.
    h.send(a, h.client(a).interact_packet(door));
    assert!(rejects(&h.take_events(a), RejectReason::InvalidCommand));
    assert!(!h.server.doors().is_open(DOOR_ID));

    // Подходим к двери (ячейка x=8, z=3): идём на +X примерно 5 м и чуть на +Z.
    for _ in 0..26 {
        h.input(a, 0, 1, EAST);
    }
    for _ in 0..6 {
        h.input(a, 0, 1, std::f32::consts::PI);
    }
    let pos = h.server.player_position(a).unwrap();
    assert!(pos.x > 6.0, "{pos}");
    h.send(a, h.client(a).interact_packet(door));
    assert!(h.server.doors().is_open(DOOR_ID));
    assert_eq!(h.client(a).door_open(door), Some(true));
    assert_eq!(
        h.client(b).door_open(door),
        Some(true),
        "второй клиент тоже увидел открытие"
    );

    // Закрыть обратно.
    h.send(a, h.client(a).interact_packet(door));
    assert!(!h.server.doors().is_open(DOOR_ID));
}

#[test]
fn locked_doors_and_missing_access_are_refused() {
    let mut h = harness();
    let a = h.join("a");
    let k = h.join("keycard");
    let door = h.client(a).door_ids()[0];
    h.server.door_state_mut(DOOR_ID).unwrap().required_access = Some("keycard".into());
    for who in [a, k] {
        for _ in 0..26 {
            h.input(who, 0, 1, EAST);
        }
        for _ in 0..6 {
            h.input(who, 0, 1, std::f32::consts::PI);
        }
    }
    h.take_events(a);
    h.send(a, h.client(a).interact_packet(door));
    assert!(rejects(&h.take_events(a), RejectReason::NotAuthorized));
    assert!(!h.server.doors().is_open(DOOR_ID));
    h.send(k, h.client(k).interact_packet(door));
    assert!(h.server.doors().is_open(DOOR_ID));
}

#[test]
fn unknown_interaction_targets_are_refused() {
    let mut h = harness();
    let a = h.join("a");
    h.send(
        a,
        h.client(a)
            .interact_packet(astrocore_core::NetworkEntityId(4242)),
    );
    assert!(rejects(&h.take_events(a), RejectReason::InvalidCommand));
}

fn wall_cmd(id: u128, x: i32, z: i32) -> WorldCommand {
    WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(id),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_x(x, z)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.2,
    })
}

#[test]
fn edits_require_rights_and_a_fresh_revision() {
    let mut h = harness();
    let player = h.join("player");
    let editor = h.join("edit-1");
    let other = h.join("edit-2");
    h.take_events(player);

    // Игрок без прав.
    let p = h
        .client(player)
        .edit_packet(wall_cmd(500_000, 3, 3))
        .unwrap();
    h.send(player, p);
    assert!(rejects(&h.take_events(player), RejectReason::NotAuthorized));
    assert!(
        h.server
            .world()
            .element(ObjectId::from_u128(500_000))
            .is_none()
    );

    // Редактор: правка применяется и приходит обоим клиентам.
    let p = h
        .client(editor)
        .edit_packet(wall_cmd(500_001, 3, 3))
        .unwrap();
    h.send(editor, p);
    for who in [player, editor, other] {
        assert!(
            h.client(who)
                .world()
                .unwrap()
                .element(ObjectId::from_u128(500_001))
                .is_some(),
            "{who:?}"
        );
        assert_eq!(
            h.client(who).world().unwrap().content_hash(),
            h.server.world().content_hash()
        );
    }

    // Второй редактор работал с устаревшей ревизией — правка отклоняется.
    let stale = encode(&ClientMessage::Edit {
        base_revision: 1,
        command: wall_cmd(500_002, 4, 4),
    })
    .unwrap();
    h.send(other, stale);
    assert!(rejects(&h.take_events(other), RejectReason::StaleRevision));
    // Некорректная команда (занятое место) — InvalidCommand.
    let p = h
        .client(other)
        .edit_packet(wall_cmd(500_003, 3, 3))
        .unwrap();
    h.send(other, p);
    assert!(rejects(&h.take_events(other), RejectReason::InvalidCommand));
}

#[test]
fn server_side_edits_update_physics_for_authoritative_movement() {
    let mut h = harness();
    let a = h.join("a");
    // Стена прямо перед игроком (он на (2.5, 2.5), идёт на +X): ребро вдоль Z на x=4.
    let cmd = WorldCommand::PlaceElement(StructureElement {
        id: ObjectId::from_u128(777_000),
        kind: StructureKind::Wall,
        placement: Placement::Edge(GridEdge::along_z(4, 2)),
        floor: FloorId(0),
        material: material(),
        thickness: 0.2,
    });
    let outs = h.server.apply_world_command(cmd).unwrap();
    h.deliver(outs);
    for _ in 0..30 {
        h.input(a, 0, 1, EAST);
    }
    assert!(
        h.server.player_position(a).unwrap().x < 4.0,
        "новая стена остановила игрока"
    );
    assert_eq!(
        h.client(a).world().unwrap().content_hash(),
        h.server.world().content_hash()
    );
    // Клиентское предсказание тоже знает о стене: коррекций почти нет.
    assert!((h.client(a).position() - h.server.player_position(a).unwrap()).length() < 0.05);
}

#[test]
fn lost_world_ops_trigger_a_snapshot_resync() {
    let mut h = harness();
    let a = h.join("a");
    h.take_events(a);
    // Теряем первую операцию, вторая обнаруживает пропуск.
    let mut seen = 0;
    h.drop_filter = Some(Box::new(move |_, m| {
        if matches!(m, ServerMessage::WorldOp(_)) {
            seen += 1;
            seen == 1
        } else {
            false
        }
    }));
    for (i, x) in [3, 4].into_iter().enumerate() {
        let outs = h
            .server
            .apply_world_command(wall_cmd(800_000 + i as u128, x, 3))
            .unwrap();
        h.deliver(outs);
    }
    assert!(
        h.take_events(a)
            .iter()
            .any(|e| matches!(e, ClientEvent::ResyncRequested))
    );
    assert!(
        h.client(a).world().is_none(),
        "до снимка реплика считается недостоверной"
    );
    // Клиент запрашивает снимок и снова синхронизируется.
    h.drop_filter = None;
    let req = h.client(a).snapshot_request_packet();
    h.send(a, req);
    assert_eq!(
        h.client(a).world().unwrap().content_hash(),
        h.server.world().content_hash()
    );
}

/// Длинная полоса пола 160×8 без стен — для проверки зоны интереса.
fn strip_world() -> astrocore_world::World {
    use astrocore_world::{FloorDef, World, WorldConfig};
    let mut w = World::new("strip", WorldConfig::default()).unwrap();
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "g".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .unwrap();
    let mut id = 1u128;
    for x in 0..160 {
        for z in 0..8 {
            w.apply(WorldCommand::PlaceElement(StructureElement {
                id: ObjectId::from_u128(id),
                kind: StructureKind::Floor,
                placement: Placement::Cell(GridCell::new(x, z)),
                floor: FloorId(0),
                material: material(),
                thickness: 0.2,
            }))
            .unwrap();
            id += 1;
        }
    }
    w.take_dirty();
    w
}

#[test]
fn interest_management_hides_far_players_and_reveals_them_again() {
    let mut cfg = Harness::default_cfg();
    cfg.interest_radius_chunks = 1;
    let mut h = Harness::with_world(strip_world(), cfg);
    let a = h.join("a");
    let b = h.join("b");
    h.ticks(2);
    assert_eq!(
        h.client(a).remote_entities().len(),
        1,
        "рядом друг с другом — видят"
    );
    h.take_events(a);

    // b уходит на восток дальше радиуса интереса (чанки по 32 м; 400 тиков ≈ 80 м).
    for _ in 0..400 {
        h.input(b, 0, 1, EAST);
    }
    h.ticks(2);
    let dist =
        horizontal(h.server.player_position(b).unwrap() - h.server.player_position(a).unwrap());
    assert!(dist > 64.0, "{dist}");
    assert_eq!(
        h.client(a).remote_entities().len(),
        0,
        "за пределами зоны интереса сущность не передаётся"
    );
    assert!(
        h.take_events(a)
            .iter()
            .any(|e| matches!(e, ClientEvent::EntityRemoved(_)))
    );

    // Возвращается — снова виден.
    for _ in 0..400 {
        h.input(b, 0, 1, WEST);
    }
    h.ticks(2);
    assert_eq!(h.client(a).remote_entities().len(), 1);
}

#[test]
fn closed_doors_hide_players_in_other_rooms_and_open_doors_reveal_them() {
    let mut h = harness();
    let a = h.join("a"); // левая комната, (2.5, 2.5)
    let b = h.join("b"); // тоже в левой комнате
    h.ticks(2);
    assert_eq!(
        h.client(a).remote_entities().len(),
        1,
        "в одной комнате видят друг друга"
    );

    // b проходит через открытую дверь в правую комнату.
    h.server.door_state_mut(DOOR_ID).unwrap().open = true;
    let outs = h
        .server
        .apply_world_command(wall_cmd(960_000, 0, 20))
        .unwrap(); // обновляет коллизию дверей
    h.deliver(outs);
    for _ in 0..5 {
        h.input(b, 0, 1, std::f32::consts::PI); // +Z к ряду двери (z≈3.5)
    }
    for _ in 0..40 {
        h.input(b, 0, 1, EAST);
    }
    assert!(
        h.server.player_position(b).unwrap().x > 9.0,
        "b в правой комнате: {}",
        h.server.player_position(b).unwrap()
    );
    h.ticks(2);
    assert_eq!(
        h.client(a).remote_entities().len(),
        1,
        "дверь открыта — обзор есть"
    );

    // Закрываем дверь (серверная логика): b пропадает из видимости a — утечки положения нет.
    h.server.door_state_mut(DOOR_ID).unwrap().open = false;
    let outs = h
        .server
        .apply_world_command(wall_cmd(960_001, 1, 20))
        .unwrap();
    h.deliver(outs);
    h.take_events(a);
    h.ticks(2);
    assert_eq!(
        h.client(a).remote_entities().len(),
        0,
        "закрытая дверь скрывает игрока в другой комнате"
    );
}

#[test]
fn entity_budget_limits_updates_per_tick_without_starving_anyone() {
    let mut cfg = Harness::default_cfg();
    cfg.entity_budget = 2;
    let mut h = Harness::new(cfg);
    let viewer = h.join("v");
    for i in 0..6 {
        h.join(&format!("p{i}"));
    }
    h.take_events(viewer);
    let mut per_tick = Vec::new();
    for _ in 0..6 {
        h.tick();
        let n = h
            .take_events(viewer)
            .iter()
            .filter(|e| matches!(e, ClientEvent::EntityUpdated(_)))
            .count();
        per_tick.push(n);
    }
    assert!(per_tick.iter().all(|&n| n <= 2), "{per_tick:?}");
    assert_eq!(
        h.client(viewer).remote_entities().len(),
        6,
        "за несколько тиков увидели всех"
    );
}

#[test]
fn flooding_is_rate_limited_and_eventually_disconnected() {
    let mut cfg = Harness::default_cfg();
    cfg.rate_capacity = 5.0;
    cfg.max_strikes = 3;
    let mut h = Harness::new(cfg);
    let a = h.join("a");
    let packet = encode(&ClientMessage::RequestSnapshot).unwrap();
    let mut limited = 0;
    let mut disconnected = false;
    for _ in 0..30 {
        let outs = h.server.on_packet(a, &packet);
        limited += outs
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    Out::Send {
                        msg: ServerMessage::Reject {
                            reason: RejectReason::RateLimited,
                            ..
                        },
                        ..
                    }
                )
            })
            .count();
        if outs.contains(&Out::Disconnect(a)) {
            disconnected = true;
            break;
        }
    }
    assert!(limited >= 3);
    assert!(disconnected, "злоупотребляющий клиент отключается");
}

#[test]
fn players_are_announced_and_removed_on_disconnect() {
    let mut h = harness();
    let a = h.join("a");
    let b = h.join("b");
    h.ticks(2);
    assert_eq!(h.client(a).remote_entities().len(), 1);
    let outs = h.server.disconnect(b);
    h.clients.remove(&b);
    h.deliver(outs);
    assert_eq!(h.client(a).remote_entities().len(), 0);
}

#[test]
fn remote_entities_are_interpolated_between_snapshots() {
    let mut h = harness();
    let a = h.join("a");
    let b = h.join("b");
    for _ in 0..10 {
        h.input(b, 0, 1, EAST);
    }
    h.ticks(2);
    let remote = h.client(a).remote_entities()[0];
    let samples: Vec<_> = (0..40)
        .map(|t| h.client(a).interpolated(remote, t as f32 * 0.5))
        .collect();
    assert!(samples.iter().all(Option::is_some));
    // Положение изменяется монотонно по X и не «прыгает» между кадрами рендера.
    let xs: Vec<f32> = samples.iter().flatten().map(|s| s.0.x).collect();
    assert!(xs.windows(2).all(|p| p[1] >= p[0] - 1e-5), "{xs:?}");
    let max_step = xs.windows(2).map(|p| p[1] - p[0]).fold(0.0f32, f32::max);
    assert!(max_step < 0.5, "шаг интерполяции {max_step}");
    assert!(
        h.client(a)
            .interpolated(astrocore_core::NetworkEntityId(9999), 1.0)
            .is_none()
    );
}

#[test]
fn clients_cannot_decode_garbage_without_error() {
    let mut h = harness();
    let a = h.join("a");
    let c = h.clients.get_mut(&a).unwrap();
    assert!(c.on_packet(&[1, 2, 3]).is_err());
    assert!(c.on_packet(&[]).is_err());
    assert!(decode::<ServerMessage>(&[0xFF; 100]).is_err());
}

#[test]
fn metrics_count_activity_and_ops_are_collected_for_persistence() {
    let mut h = harness();
    let a = h.join("edit-a");
    h.input(a, 0, 1, WEST);
    let p = h.client(a).edit_packet(wall_cmd(321_000, 3, 3)).unwrap();
    h.send(a, p);
    let bad = h.server.on_packet(a, &[9, 9]);
    assert!(!bad.is_empty());
    let m = h.server.metrics();
    assert!(m.ticks >= 1 && m.packets_in >= 4 && m.messages_out > 0);
    assert_eq!((m.edits_applied, m.inputs_applied), (1, 1));
    assert!(m.rejected_packets >= 1);
    let ops = h.server.take_applied_ops();
    assert_eq!(ops.len(), 1);
    assert!(
        h.server.take_applied_ops().is_empty(),
        "забираются один раз"
    );
}

mod terrain {
    use astrocore_math::glam::Vec2;
    use astrocore_terrain::{Terrain, TerrainEdit};
    use astrocore_world::{FloorDef, World, WorldConfig};

    use super::*;

    /// Мир без построек и холм высотой 3 м вокруг (30, 0).
    fn hill_harness() -> Harness {
        let mut w = World::new("hills", WorldConfig::default()).unwrap();
        w.apply(WorldCommand::DefineFloor(FloorDef { id: FloorId(0), name: "g".into(), base_y: 0.0, height: 3.0 })).unwrap();
        let mut t = Terrain::new(1.0, 16, 0.0).unwrap();
        let e = t.brush_raise(Vec2::new(30.0, 0.0), 14.0, 3.0);
        t.apply(&e).unwrap();
        let cfg = crate::server::ServerNetConfig { spawn: Vec3::new(14.0, 2.0, 0.0), ..Default::default() };
        Harness::with_server(crate::server::GameServer::new(w, cfg, Box::new(crate::testkit::ByBuild)).with_terrain(t))
    }

    #[test]
    fn terrain_arrives_with_the_snapshot_and_prediction_on_hills_matches_the_server() {
        let mut h = hill_harness();
        let a = h.join("a");
        let c = h.client(a);
        assert_eq!(c.terrain().unwrap().content_hash(), h.server.terrain().unwrap().content_hash());
        assert_eq!(c.terrain_serial(), 1);
        h.take_events(a);
        // Подъём на холм: сервер и клиент считают движение по одной и той же земле.
        for _ in 0..80 {
            h.input(a, 0, 1, EAST);
        }
        h.ticks(2);
        let server = h.server.player_position(a).unwrap();
        let ground = h.server.terrain().unwrap().sample(server.x, server.z);
        assert!(server.x > 28.0 && server.y > 2.4, "поднялся на холм: {server}");
        assert!((server.y - ground).abs() < 0.05, "стоит на земле: {server} / {ground}");
        assert!(!h.take_events(a).iter().any(|e| matches!(e, ClientEvent::Corrected { .. })));
        assert!((h.client(a).position() - server).length() < 1e-3);
    }

    #[test]
    fn terrain_edits_need_rights_a_fresh_revision_and_valid_data() {
        let mut h = hill_harness();
        let player = h.join("player");
        let editor = h.join("edit-1");
        let rev = h.client(editor).terrain().unwrap().revision();
        let edit = h.server.terrain().unwrap().brush_raise(Vec2::new(5.0, 5.0), 4.0, 2.0);

        h.send(player, h.client(player).edit_terrain_packet(&edit).unwrap());
        assert!(rejects(&h.take_events(player), RejectReason::NotAuthorized));
        assert_eq!(h.server.terrain().unwrap().revision(), rev);

        h.send(editor, h.client(editor).edit_terrain_packet(&edit).unwrap());
        for who in [player, editor] {
            assert_eq!(h.client(who).terrain().unwrap().content_hash(), h.server.terrain().unwrap().content_hash(), "{who:?}");
            assert!(h.client(who).terrain().unwrap().sample(5.0, 5.0) > 1.5);
        }
        assert_eq!(h.server.take_applied_terrain_ops().len(), 1);

        // Правка, собранная для старой ревизии: клиент `player` уже на новой, делаем устаревший пакет вручную.
        let stale = encode(&ClientMessage::EditTerrain { base_revision: rev, data: postcard::to_stdvec(&edit).unwrap() }).unwrap();
        h.send(editor, stale);
        assert!(rejects(&h.take_events(editor), RejectReason::StaleRevision));
        // Мусор вместо правки.
        let fresh = h.server.terrain().unwrap().revision();
        h.send(editor, encode(&ClientMessage::EditTerrain { base_revision: fresh, data: vec![0xFF; 20] }).unwrap());
        assert!(rejects(&h.take_events(editor), RejectReason::InvalidCommand));
        // Правка, не соответствующая состоянию (чужие прежние высоты).
        let bad = TerrainEdit { changes: vec![(0, 0, 123.0, 1.0)] };
        h.send(editor, encode(&ClientMessage::EditTerrain { base_revision: fresh, data: postcard::to_stdvec(&bad).unwrap() }).unwrap());
        assert!(rejects(&h.take_events(editor), RejectReason::InvalidCommand));
        assert_eq!(h.server.terrain().unwrap().revision(), fresh, "рельеф не тронут");
    }

    #[test]
    fn a_lost_terrain_op_triggers_a_resync() {
        let mut h = hill_harness();
        let a = h.join("edit-a");
        h.take_events(a);
        let mut seen = 0;
        h.drop_filter = Some(Box::new(move |_, m| {
            if matches!(m, ServerMessage::TerrainOp { .. }) {
                seen += 1;
                seen == 1
            } else {
                false
            }
        }));
        for x in [1.0f32, 9.0] {
            let e = h.server.terrain().unwrap().brush_raise(Vec2::new(x, 20.0), 3.0, 1.0);
            let outs = h.server.apply_terrain_edit(&e).unwrap();
            h.deliver(outs);
        }
        assert!(h.take_events(a).iter().any(|e| matches!(e, ClientEvent::ResyncRequested)));
        assert!(h.client(a).terrain().is_none());
        h.drop_filter = None;
        h.send(a, h.client(a).snapshot_request_packet());
        assert_eq!(h.client(a).terrain().unwrap().content_hash(), h.server.terrain().unwrap().content_hash());
    }

    #[test]
    fn worlds_without_terrain_reject_terrain_edits() {
        let mut h = harness();
        let a = h.join("edit-a");
        h.send(a, encode(&ClientMessage::EditTerrain { base_revision: 0, data: vec![] }).unwrap());
        assert!(rejects(&h.take_events(a), RejectReason::InvalidCommand));
        assert!(h.client(a).terrain().is_none());
        assert!(h.server.apply_terrain_edit(&TerrainEdit::default()).is_err());
    }
}
