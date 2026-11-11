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
        w.apply(WorldCommand::DefineFloor(FloorDef {
            id: FloorId(0),
            name: "g".into(),
            base_y: 0.0,
            height: 3.0,
        }))
        .unwrap();
        let mut t = Terrain::new(1.0, 16, 0.0).unwrap();
        let e = t.brush_raise(Vec2::new(30.0, 0.0), 14.0, 3.0);
        t.apply(&e).unwrap();
        let cfg = crate::server::ServerNetConfig {
            spawn: Vec3::new(14.0, 2.0, 0.0),
            ..Default::default()
        };
        Harness::with_server(
            crate::server::GameServer::new(w, cfg, Box::new(crate::testkit::ByBuild))
                .with_terrain(t),
        )
    }

    #[test]
    fn terrain_arrives_with_the_snapshot_and_prediction_on_hills_matches_the_server() {
        let mut h = hill_harness();
        let a = h.join("a");
        let c = h.client(a);
        assert_eq!(
            c.terrain().unwrap().content_hash(),
            h.server.terrain().unwrap().content_hash()
        );
        assert_eq!(c.terrain_serial(), 1);
        h.take_events(a);
        // Подъём на холм: сервер и клиент считают движение по одной и той же земле.
        for _ in 0..80 {
            h.input(a, 0, 1, EAST);
        }
        h.ticks(2);
        let server = h.server.player_position(a).unwrap();
        let ground = h.server.terrain().unwrap().sample(server.x, server.z);
        assert!(
            server.x > 28.0 && server.y > 2.4,
            "поднялся на холм: {server}"
        );
        assert!(
            (server.y - ground).abs() < 0.05,
            "стоит на земле: {server} / {ground}"
        );
        assert!(
            !h.take_events(a)
                .iter()
                .any(|e| matches!(e, ClientEvent::Corrected { .. }))
        );
        assert!((h.client(a).position() - server).length() < 1e-3);
    }

    #[test]
    fn terrain_edits_need_rights_a_fresh_revision_and_valid_data() {
        let mut h = hill_harness();
        let player = h.join("player");
        let editor = h.join("edit-1");
        let rev = h.client(editor).terrain().unwrap().revision();
        let edit = h
            .server
            .terrain()
            .unwrap()
            .brush_raise(Vec2::new(5.0, 5.0), 4.0, 2.0);

        h.send(player, h.client(player).edit_terrain_packet(&edit).unwrap());
        assert!(rejects(&h.take_events(player), RejectReason::NotAuthorized));
        assert_eq!(h.server.terrain().unwrap().revision(), rev);

        h.send(editor, h.client(editor).edit_terrain_packet(&edit).unwrap());
        for who in [player, editor] {
            assert_eq!(
                h.client(who).terrain().unwrap().content_hash(),
                h.server.terrain().unwrap().content_hash(),
                "{who:?}"
            );
            assert!(h.client(who).terrain().unwrap().sample(5.0, 5.0) > 1.5);
        }
        assert_eq!(h.server.take_applied_terrain_ops().len(), 1);

        // Правка, собранная для старой ревизии: клиент `player` уже на новой, делаем устаревший пакет вручную.
        let stale = encode(&ClientMessage::EditTerrain {
            base_revision: rev,
            data: postcard::to_stdvec(&edit).unwrap(),
        })
        .unwrap();
        h.send(editor, stale);
        assert!(rejects(&h.take_events(editor), RejectReason::StaleRevision));
        // Мусор вместо правки.
        let fresh = h.server.terrain().unwrap().revision();
        h.send(
            editor,
            encode(&ClientMessage::EditTerrain {
                base_revision: fresh,
                data: vec![0xFF; 20],
            })
            .unwrap(),
        );
        assert!(rejects(
            &h.take_events(editor),
            RejectReason::InvalidCommand
        ));
        // Правка, не соответствующая состоянию (чужие прежние высоты).
        let bad = TerrainEdit {
            changes: vec![(0, 0, 123.0, 1.0)],
        };
        h.send(
            editor,
            encode(&ClientMessage::EditTerrain {
                base_revision: fresh,
                data: postcard::to_stdvec(&bad).unwrap(),
            })
            .unwrap(),
        );
        assert!(rejects(
            &h.take_events(editor),
            RejectReason::InvalidCommand
        ));
        assert_eq!(
            h.server.terrain().unwrap().revision(),
            fresh,
            "рельеф не тронут"
        );
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
            let e = h
                .server
                .terrain()
                .unwrap()
                .brush_raise(Vec2::new(x, 20.0), 3.0, 1.0);
            let outs = h.server.apply_terrain_edit(&e).unwrap();
            h.deliver(outs);
        }
        assert!(
            h.take_events(a)
                .iter()
                .any(|e| matches!(e, ClientEvent::ResyncRequested))
        );
        assert!(h.client(a).terrain().is_none());
        h.drop_filter = None;
        h.send(a, h.client(a).snapshot_request_packet());
        assert_eq!(
            h.client(a).terrain().unwrap().content_hash(),
            h.server.terrain().unwrap().content_hash()
        );
    }

    #[test]
    fn worlds_without_terrain_reject_terrain_edits() {
        let mut h = harness();
        let a = h.join("edit-a");
        h.send(
            a,
            encode(&ClientMessage::EditTerrain {
                base_revision: 0,
                data: vec![],
            })
            .unwrap(),
        );
        assert!(rejects(&h.take_events(a), RejectReason::InvalidCommand));
        assert!(h.client(a).terrain().is_none());
        assert!(
            h.server
                .apply_terrain_edit(&TerrainEdit::default())
                .is_err()
        );
    }
}

mod hooks {
    use std::sync::{Arc, Mutex};

    use astrocore_runtime::modules::{GameModule, HookContext, HookEvent, ModuleHost};

    use super::*;
    use crate::server::GameServer;
    use crate::testkit::{ByBuild, test_world};

    struct Tap(Arc<Mutex<Vec<HookEvent>>>);

    impl GameModule for Tap {
        fn id(&self) -> &str {
            "test.tap"
        }
        fn handle(&mut self, _ctx: &mut HookContext<'_>, event: &HookEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn engine_raises_universal_hooks() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut host = ModuleHost::new();
        host.register(Box::new(Tap(seen.clone()))).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        let door = h.client(a).door_ids()[0];
        for _ in 0..26 {
            h.input(a, 0, 1, EAST);
        }
        for _ in 0..6 {
            h.input(a, 0, 1, std::f32::consts::PI);
        }
        h.send(a, h.client(a).interact_packet(door));
        let outs = h
            .server
            .notify_chunk_loaded(astrocore_math::ChunkCoord::new(3, -2));
        h.deliver(outs);
        let events = seen.lock().unwrap().clone();
        let has = |f: &dyn Fn(&HookEvent) -> bool| events.iter().any(f);
        assert!(has(&|e| matches!(e, HookEvent::EntitySpawned { .. })));
        assert!(has(&|e| matches!(e, HookEvent::Interact { .. })));
        assert!(has(&|e| matches!(
            e,
            HookEvent::PortalChanged { open: true, .. }
        )));
        assert!(has(&|e| matches!(
            e,
            HookEvent::ComponentChanged { component, .. } if component == "door.open"
        )));
        assert!(has(&|e| matches!(
            e,
            HookEvent::ChunkLoaded { chunk } if *chunk == astrocore_math::ChunkCoord::new(3, -2)
        )));
        // Игрок упёрся в закрытую дверь: контакт засчитан ровно один раз.
        let collisions = events
            .iter()
            .filter(|e| matches!(e, HookEvent::Collision { .. }))
            .count();
        assert_eq!(collisions, 1, "{events:?}");
    }
}

#[test]
fn metrics_are_exported_as_text() {
    let mut h = harness();
    let a = h.join("a");
    h.input(a, 0, 1, 0.0);
    let text = h.server.metrics_text();
    for key in [
        "astrocore_ticks ",
        "astrocore_players 1",
        "astrocore_tick_micros_max ",
        "astrocore_world_revision ",
    ] {
        assert!(text.contains(key), "нет {key}: {text}");
    }
    assert!(h.server.metrics().tick_micros_max >= h.server.metrics().tick_micros_last);
}

mod components {
    use astrocore_api::{Interest, ReplicatedKind, ReplicationChannel, ReplicationRegistry};

    use super::*;
    use crate::server::GameServer;
    use crate::testkit::{ByBuild, test_world};

    fn kind(name: &str, channel: ReplicationChannel, interest: Interest) -> ReplicatedKind {
        ReplicatedKind {
            name: name.into(),
            channel,
            interest,
            max_bytes: 16,
        }
    }

    fn harness_with_kinds() -> Harness {
        let mut reg = ReplicationRegistry::new();
        reg.register(kind(
            "game.tag",
            ReplicationChannel::ReliableOrdered,
            Interest::Everyone,
        ))
        .unwrap();
        reg.register(kind(
            "game.aura",
            ReplicationChannel::UnreliableSequenced,
            Interest::Radius(5.0),
        ))
        .unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_replication(reg);
        Harness::with_server(server)
    }

    fn net_of(h: &Harness, c: ConnId) -> astrocore_core::NetworkEntityId {
        h.client(c).net_id().unwrap()
    }

    #[test]
    fn values_reach_every_client_and_late_joiners() {
        let mut h = harness_with_kinds();
        let a = h.join("a");
        let b = h.join("b");
        let me = net_of(&h, a);
        let outs = h
            .server
            .set_component(me, "game.tag", b"red".to_vec())
            .unwrap();
        h.deliver(outs);
        assert_eq!(h.client(a).component(me, "game.tag"), Some(&b"red"[..]));
        assert_eq!(h.client(b).component(me, "game.tag"), Some(&b"red"[..]));
        assert!(h.take_events(b).iter().any(
            |e| matches!(e, ClientEvent::ComponentChanged { kind, .. } if kind == "game.tag")
        ));
        // Обновление заменяет значение.
        let outs = h
            .server
            .set_component(me, "game.tag", b"blue".to_vec())
            .unwrap();
        h.deliver(outs);
        assert_eq!(h.client(b).component(me, "game.tag"), Some(&b"blue"[..]));
        // Вошедший позже получает текущие значения вместе со снимком.
        let c = h.join("c");
        assert_eq!(h.client(c).component(me, "game.tag"), Some(&b"blue"[..]));
    }

    #[test]
    fn unknown_oversized_and_orphan_values_are_refused() {
        let mut h = harness_with_kinds();
        let a = h.join("a");
        let me = net_of(&h, a);
        assert!(h.server.set_component(me, "game.nope", vec![1]).is_err());
        assert!(h.server.set_component(me, "game.tag", vec![0; 17]).is_err());
        assert!(
            h.server
                .set_component(astrocore_core::NetworkEntityId(9999), "game.tag", vec![1])
                .is_err()
        );
        assert_eq!(h.client(a).component(me, "game.tag"), None);
    }

    #[test]
    fn radius_interest_limits_recipients_and_disconnect_clears_values() {
        let mut h = harness_with_kinds();
        let a = h.join("a");
        let b = h.join("b");
        let me = net_of(&h, a);
        // Игрок b уходит далеко от a: на +X идти по 4 м/с.
        for _ in 0..60 {
            h.input(b, 0, 1, EAST);
        }
        let near_far = h.server.set_component(me, "game.aura", vec![7]).unwrap();
        let targets: Vec<ConnId> = near_far
            .iter()
            .filter_map(|o| match o {
                Out::Send { to, .. } => Some(*to),
                _ => None,
            })
            .collect();
        assert!(
            targets.contains(&a),
            "владелец всегда получает своё значение"
        );
        assert!(!targets.contains(&b), "далёкий игрок вне радиуса");
        h.deliver(near_far);
        // После выхода владельца его значения забываются: новый игрок их не получит.
        let outs = h.server.disconnect(a);
        h.deliver(outs);
        let c = h.join("c");
        assert_eq!(h.client(c).component(me, "game.aura"), None);
    }

    #[test]
    fn owner_only_values_never_reach_other_clients_even_through_a_snapshot() {
        let mut reg = ReplicationRegistry::new();
        reg.register(kind(
            "game.secret",
            ReplicationChannel::ReliableOrdered,
            Interest::Owner,
        ))
        .unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_replication(reg);
        let mut h = Harness::with_server(server);
        let (a, b) = (h.join("a"), h.join("b"));
        let me = net_of(&h, a);
        let outs = h
            .server
            .set_component(me, "game.secret", b"x".to_vec())
            .unwrap();
        h.deliver(outs);
        assert_eq!(
            h.client(a).component(me, "game.secret"),
            Some(&b"x"[..]),
            "владелец видит"
        );
        assert_eq!(
            h.client(b).component(me, "game.secret"),
            None,
            "другой — нет"
        );
        // Новый игрок и повторный запрос снимка тоже не получают чужого.
        let c = h.join("c");
        assert_eq!(h.client(c).component(me, "game.secret"), None);
        let p = h.client(b).snapshot_request_packet();
        h.send(b, p);
        assert_eq!(h.client(b).component(me, "game.secret"), None);
        // А владелец после своего запроса снимка значение получает.
        let p = h.client(a).snapshot_request_packet();
        h.send(a, p);
        assert_eq!(h.client(a).component(me, "game.secret"), Some(&b"x"[..]));
    }

    #[test]
    fn stale_sequence_numbers_are_ignored_by_the_client() {
        let mut h = harness_with_kinds();
        let a = h.join("a");
        let me = net_of(&h, a);
        let mk = |seq: u32, v: u8| {
            encode(&ServerMessage::ComponentDelta {
                net: me,
                kind: "game.aura".into(),
                seq,
                reliable: false,
                data: vec![v],
            })
            .unwrap()
        };
        let evs = h
            .clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&mk(5, 50))
            .unwrap();
        assert_eq!(evs.len(), 1);
        // Запоздавший пакет со старым номером не затирает новое значение.
        let evs = h
            .clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&mk(3, 30))
            .unwrap();
        assert!(evs.is_empty());
        assert_eq!(h.client(a).component(me, "game.aura"), Some(&[50u8][..]));
    }

    #[test]
    fn protocol_limits_and_udp_channel_selection() {
        let bad = |kind: &str, len: usize| {
            encode(&ServerMessage::ComponentDelta {
                net: astrocore_core::NetworkEntityId(1),
                kind: kind.into(),
                seq: 1,
                reliable: true,
                data: vec![0; len],
            })
        };
        assert!(bad("ok", 8).is_ok());
        assert!(bad("", 1).is_err());
        assert!(bad(&"x".repeat(65), 1).is_err());
        assert!(bad("ok", astrocore_protocol::MAX_COMPONENT_BYTES + 1).is_err());
        let msg = |reliable| ServerMessage::ComponentDelta {
            net: astrocore_core::NetworkEntityId(1),
            kind: "k".into(),
            seq: 1,
            reliable,
            data: vec![1],
        };
        assert!(crate::udp::server_message_is_reliable(&msg(true), 20));
        assert!(!crate::udp::server_message_is_reliable(&msg(false), 20));
        assert!(crate::udp::server_message_is_reliable(&msg(false), 5000));
    }
}

mod content {
    use astrocore_protocol::CONTENT_PART_BYTES;
    use sha2::{Digest, Sha256};

    use super::*;

    fn blob(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn pump(h: &mut Harness, ticks: usize) {
        for _ in 0..ticks {
            h.tick();
        }
    }

    #[test]
    fn content_is_delivered_in_parts_by_hash_and_verified() {
        let mut h = harness();
        let big = blob(CONTENT_PART_BYTES * 5 + 123);
        let hash = h.server.register_content(big.clone()).unwrap();
        let expect: [u8; 32] = Sha256::digest(&big).into();
        assert_eq!(hash, expect);
        let a = h.join("a");
        h.send(a, h.client(a).request_content_packet(hash));
        pump(&mut h, 12);
        assert_eq!(h.client(a).content(&hash), Some(&big[..]));
        assert!(
            h.take_events(a)
                .iter()
                .any(|e| matches!(e, ClientEvent::ContentReceived { hash: x } if *x == hash))
        );
    }

    #[test]
    fn per_tick_budget_keeps_gameplay_flowing_and_unknown_hash_is_reported() {
        let mut cfg = Harness::default_cfg();
        cfg.content_bytes_per_tick = CONTENT_PART_BYTES; // одна часть за тик
        let mut h = Harness::new(cfg);
        let big = blob(CONTENT_PART_BYTES * 10);
        let hash = h.server.register_content(big.clone()).unwrap();
        let a = h.join("a");
        h.send(a, h.client(a).request_content_packet(hash));
        pump(&mut h, 3);
        assert_eq!(
            h.client(a).content(&hash),
            None,
            "загрузка идёт частями, не одним залпом"
        );
        // Пока грузится, игра отвечает на ввод.
        let before = h.server.player_position(a).unwrap();
        h.input(a, 0, 1, EAST);
        assert!(h.server.player_position(a).unwrap().x > before.x);
        pump(&mut h, 12);
        assert_eq!(h.client(a).content(&hash), Some(&big[..]));
        // Неизвестный хеш.
        let missing = [9u8; 32];
        h.send(a, h.client(a).request_content_packet(missing));
        assert!(
            h.take_events(a)
                .iter()
                .any(|e| matches!(e, ClientEvent::ContentFailed { hash: x } if *x == missing))
        );
    }

    #[test]
    fn corrupted_or_inconsistent_parts_never_become_content() {
        let mut h = harness();
        let a = h.join("a");
        let data = blob(100);
        let hash: [u8; 32] = Sha256::digest(&data).into();
        let part = |index: u32, total: u32, bytes: &[u8]| {
            encode(&ServerMessage::ContentPart {
                hash,
                index,
                total,
                data: bytes.to_vec(),
            })
            .unwrap()
        };
        // Подмена данных: хеш не сходится.
        let mut forged = data.clone();
        forged[0] ^= 1;
        let evs = h
            .clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&part(0, 1, &forged))
            .unwrap();
        assert!(matches!(evs[0], ClientEvent::ContentFailed { .. }));
        assert_eq!(h.client(a).content(&hash), None);
        // Несогласованное число частей.
        h.clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&part(0, 2, &data[..50]))
            .unwrap();
        let evs = h
            .clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&part(1, 3, &data[50..]))
            .unwrap();
        assert!(matches!(evs[0], ClientEvent::ContentFailed { .. }));
        // Корректная сборка в обратном порядке частей.
        h.clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&part(1, 2, &data[50..]))
            .unwrap();
        let evs = h
            .clients
            .get_mut(&a)
            .unwrap()
            .on_packet(&part(0, 2, &data[..50]))
            .unwrap();
        assert!(matches!(evs[0], ClientEvent::ContentReceived { .. }));
        assert_eq!(h.client(a).content(&hash), Some(&data[..]));
        // Недопустимые параметры не кодируются вовсе.
        assert!(
            encode(&ServerMessage::ContentPart {
                hash,
                index: 2,
                total: 2,
                data: vec![]
            })
            .is_err()
        );
        assert!(
            encode(&ServerMessage::ContentPart {
                hash,
                index: 0,
                total: 0,
                data: vec![]
            })
            .is_err()
        );
        assert!(h.server.register_content(Vec::new()).is_err());
    }

    #[test]
    fn request_flood_is_bounded() {
        let mut cfg = Harness::default_cfg();
        cfg.content_queue_max = 2;
        cfg.content_bytes_per_tick = 1; // почти не отправляем: очередь копится
        let mut h = Harness::new(cfg);
        let hashes: Vec<[u8; 32]> = (0..6)
            .map(|i| {
                h.server
                    .register_content(blob(CONTENT_PART_BYTES * 2 + i))
                    .unwrap()
            })
            .collect();
        let a = h.join("a");
        for hash in &hashes {
            h.send(a, h.client(a).request_content_packet(*hash));
        }
        let queued = h.server.content_queue_len(a);
        assert!(queued <= 2 * 3 + 3, "очередь ограничена: {queued}");
    }
}

mod game_commands {
    use astrocore_api::{Interest, ReplicatedKind, ReplicationChannel, ReplicationRegistry};
    use astrocore_runtime::modules::{GameModule, HookContext, HookEvent, ModuleHost};

    use super::*;
    use crate::server::GameServer;
    use crate::testkit::{ByBuild, test_world};

    /// Модуль-счётчик: команда «count.add» увеличивает значение и реплицирует его.
    struct Counter(u32);

    impl GameModule for Counter {
        fn id(&self) -> &str {
            "test.counter"
        }
        fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
            if let HookEvent::Command { actor, kind, data } = event
                && kind == "count.add"
            {
                self.0 += u32::from(data.first().copied().unwrap_or(0));
                ctx.set_component(*actor, "test.count", self.0.to_le_bytes().to_vec());
            }
        }
    }

    /// Отвечает позицией игрока: проверка `position_of`.
    struct Where;

    impl GameModule for Where {
        fn id(&self) -> &str {
            "test.where"
        }
        fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
            if let HookEvent::Command { actor, kind, .. } = event
                && kind == "where"
                && let Some(p) = ctx.position_of(*actor)
            {
                ctx.set_component(*actor, "test.count", (p.x as u32).to_le_bytes().to_vec());
            }
        }
    }

    fn harness() -> Harness {
        let mut host = ModuleHost::new();
        host.register(Box::new(Counter(0))).unwrap();
        host.register(Box::new(Where)).unwrap();
        host.finalize().unwrap();
        let mut reg = ReplicationRegistry::new();
        reg.register(ReplicatedKind {
            name: "test.count".into(),
            channel: ReplicationChannel::ReliableOrdered,
            interest: Interest::Everyone,
            max_bytes: 8,
        })
        .unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host)
            .with_replication(reg);
        Harness::with_server(server)
    }

    #[test]
    fn commands_reach_the_module_and_replicated_answers_reach_everyone() {
        let mut h = harness();
        let a = h.join("a");
        let b = h.join("b");
        let me = h.client(a).net_id().unwrap();
        let packet = h.client(a).command_packet("count.add", vec![5]).unwrap();
        h.send(a, packet);
        let packet = h.client(a).command_packet("count.add", vec![2]).unwrap();
        h.send(a, packet);
        assert_eq!(
            h.client(b).component(me, "test.count"),
            Some(&7u32.to_le_bytes()[..])
        );
        assert_eq!(
            h.client(a).component(me, "test.count"),
            Some(&7u32.to_le_bytes()[..])
        );
    }

    #[test]
    fn a_frozen_player_does_not_move_until_released() {
        struct Freezer;
        impl GameModule for Freezer {
            fn id(&self) -> &str {
                "test.freezer"
            }
            fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
                if let HookEvent::Command { actor, kind, .. } = event {
                    match kind.as_str() {
                        "freeze" => ctx.freeze(*actor, true),
                        "thaw" => ctx.freeze(*actor, false),
                        _ => {}
                    }
                }
            }
        }
        let mut host = ModuleHost::new();
        host.register(Box::new(Freezer)).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        let flat = |h: &Harness| {
            let p = h.client(a).position();
            (p.x, p.z)
        };
        let start = flat(&h);
        let packet = h.client(a).command_packet("freeze", vec![]).unwrap();
        h.send(a, packet);
        for _ in 0..10 {
            h.input(a, 0, 1, 0.0);
        }
        assert_eq!(flat(&h), start, "заморожен — на месте");
        let packet = h.client(a).command_packet("thaw", vec![]).unwrap();
        h.send(a, packet);
        for _ in 0..10 {
            h.input(a, 0, 1, 0.0);
        }
        assert_ne!(flat(&h), start, "разморожен — пошёл");
    }

    #[test]
    fn a_ghost_flies_without_gravity_and_a_teleport_reaches_the_client_at_once() {
        struct Spooky;
        impl GameModule for Spooky {
            fn id(&self) -> &str {
                "test.spooky"
            }
            fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
                if let HookEvent::Command { actor, kind, .. } = event {
                    match kind.as_str() {
                        "ghost" => ctx.set_ghost(*actor, true),
                        "human" => ctx.set_ghost(*actor, false),
                        "tp" => {
                            ctx.teleport(*actor, astrocore_math::glam::Vec3::new(2.0, 2.5, 2.0))
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut host = ModuleHost::new();
        host.register(Box::new(Spooky)).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        h.ticks(5);
        let send = |h: &mut Harness, kind: &str| {
            let p = h.client(a).command_packet(kind, vec![]).unwrap();
            h.send(a, p);
        };
        send(&mut h, "tp");
        h.ticks(3);
        let p = h.client(a).position();
        assert!((p.y - 2.5).abs() < 1e-4, "телепорт дошёл до клиента: {p:?}");
        send(&mut h, "human");
        for _ in 0..10 {
            h.input(a, 0, 0, 0.0);
        }
        assert!(h.client(a).position().y < 2.5, "обычный игрок падает");
        // Призрак: поднимаем на 2,5 м — и он висит.
        send(&mut h, "tp");
        h.ticks(3);
        send(&mut h, "ghost");
        h.ticks(2);
        h.client_noclip(a, true);
        for _ in 0..10 {
            h.input(a, 0, 1, 0.0);
        }
        let g = h.client(a).position();
        assert!((g.y - 2.5).abs() < 1e-4, "призрак не падает: {g:?}");
        assert!(g.z < 2.0 - 1.0, "и летит вперёд: {g:?}");
    }

    #[test]
    fn modules_receive_a_tick_with_the_tick_duration_every_server_tick() {
        use std::sync::{Arc, Mutex};
        struct Clock(Arc<Mutex<Vec<(u32, f32)>>>);
        impl GameModule for Clock {
            fn id(&self) -> &str {
                "test.clock"
            }
            fn handle(&mut self, _: &mut HookContext<'_>, event: &HookEvent) {
                if let HookEvent::Tick { tick, dt } = event {
                    self.0.lock().unwrap().push((*tick, *dt));
                }
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut host = ModuleHost::new();
        host.register(Box::new(Clock(seen.clone()))).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        h.ticks(5);
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 5, "{seen:?}");
        assert!(
            seen.windows(2).all(|w| w[1].0 == w[0].0 + 1),
            "тики идут подряд"
        );
        let dt = 1.0 / f32::from(Harness::default_cfg().tick_hz);
        assert!(seen.iter().all(|(_, d)| (*d - dt).abs() < 1e-6));
    }

    #[test]
    fn modules_learn_when_a_player_leaves_and_where_they_were() {
        use std::sync::{Arc, Mutex};
        struct Leave(Arc<Mutex<Vec<(astrocore_core::NetworkEntityId, f32)>>>);
        impl GameModule for Leave {
            fn id(&self) -> &str {
                "test.leave"
            }
            fn handle(&mut self, _: &mut HookContext<'_>, event: &HookEvent) {
                if let HookEvent::PlayerLeft { actor, pos } = event {
                    self.0.lock().unwrap().push((*actor, pos.x));
                }
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut host = ModuleHost::new();
        host.register(Box::new(Leave(seen.clone()))).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        let me = h.client(a).net_id().unwrap();
        let outs = h.server.disconnect(a);
        h.deliver(outs);
        assert_eq!(*seen.lock().unwrap(), vec![(me, 2.5)]);
    }

    /// Модуль управляет дверью по команде и пытается зациклить сам себя через PortalChanged.
    struct DoorMan;

    impl GameModule for DoorMan {
        fn id(&self) -> &str {
            "test.doorman"
        }
        fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
            use astrocore_runtime::modules::DoorAction;
            match event {
                HookEvent::Command { kind, data, .. } if kind == "door" => {
                    let force = data.first() == Some(&1);
                    if let Some(id) = ctx
                        .world
                        .elements()
                        .find(|e| {
                            matches!(
                                e.kind,
                                astrocore_world::StructureKind::Opening(
                                    astrocore_world::OpeningKind::Door
                                )
                            )
                        })
                        .map(|e| e.id)
                    {
                        ctx.request_door(id, DoorAction::Toggle, force);
                    }
                }
                // Каждое изменение двери вызывает новое — без ограничителя это бесконечно.
                HookEvent::PortalChanged { element, .. } => {
                    ctx.request_door(*element, DoorAction::Toggle, true)
                }
                _ => {}
            }
        }
    }

    #[test]
    fn modules_control_doors_through_the_engine_and_cannot_loop_forever() {
        let mut host = ModuleHost::new();
        host.register(Box::new(DoorMan)).unwrap();
        host.finalize().unwrap();
        let server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        let b = h.join("b");
        let door = h.client(a).door_ids()[0];
        let before = h.client(b).door_open(door);
        let p = h.client(a).command_packet("door", vec![0]).unwrap();
        h.send(a, p); // не зацикливается и возвращается
        // Состояние у второго клиента согласовано с сервером, а не «залипло» посреди цепочки.
        let element = h.client(b).door_element(door).unwrap();
        assert_eq!(
            h.client(b).door_open(door),
            Some(h.server.doors().is_open(element))
        );
        assert!(before.is_some());
        assert!(h.server.metrics().door_toggles <= 5, "цепочка ограничена");
    }

    #[test]
    fn locked_doors_ignore_ordinary_requests_but_yield_to_forced_ones() {
        struct Once;
        impl GameModule for Once {
            fn id(&self) -> &str {
                "test.once"
            }
            fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
                use astrocore_runtime::modules::DoorAction;
                if let HookEvent::Command { kind, data, .. } = event
                    && kind == "door"
                    && let Some(id) = ctx
                        .world
                        .elements()
                        .find(|e| {
                            matches!(
                                e.kind,
                                astrocore_world::StructureKind::Opening(
                                    astrocore_world::OpeningKind::Door
                                )
                            )
                        })
                        .map(|e| e.id)
                {
                    ctx.request_door(id, DoorAction::Open, data.first() == Some(&1));
                }
            }
        }
        let mut host = ModuleHost::new();
        host.register(Box::new(Once)).unwrap();
        host.finalize().unwrap();
        let mut server = GameServer::new(test_world(), Harness::default_cfg(), Box::new(ByBuild))
            .with_modules(host);
        server
            .door_state_mut(crate::testkit::DOOR_ID)
            .unwrap()
            .locked = true;
        let mut h = Harness::with_server(server);
        let a = h.join("a");
        let p = h.client(a).command_packet("door", vec![0]).unwrap();
        h.send(a, p);
        assert!(
            !h.server.doors().is_open(crate::testkit::DOOR_ID),
            "замок держит"
        );
        let p = h.client(a).command_packet("door", vec![1]).unwrap();
        h.send(a, p);
        assert!(
            h.server.doors().is_open(crate::testkit::DOOR_ID),
            "взлом открывает"
        );
        let door = h.client(a).door_ids()[0];
        assert_eq!(
            h.client(a).door_open(door),
            Some(true),
            "клиент видит открытую дверь"
        );
    }

    #[test]
    fn modules_can_check_whether_a_path_is_obstructed() {
        use astrocore_math::glam::Vec3;
        // Проверка прямо на контексте с коллизией тестового мира (стены, закрытая дверь).
        let world = test_world();
        let bw = astrocore_physics::BoxWorld::from_world(&world);
        let ctx = HookContext::new(&world).with_physics(&bw);
        // Сквозь любую статическую коробку мира путь перекрыт, и расстояние обрезается по её грани.
        let (_, b) = *bw
            .static_boxes()
            .next()
            .expect("в тестовом мире есть коллизия");
        let c = b.center();
        let half = b.size() * 0.5;
        let (from, to) = (c - Vec3::X * (half.x + 1.0), c + Vec3::X * (half.x + 1.0));
        assert!(!ctx.is_unobstructed(from, to));
        let cut = ctx.unobstructed_distance(from, to);
        assert!(
            cut > 0.0 && cut < (to - from).length() - 0.5,
            "упёрлись раньше цели: {cut}"
        );
        // Через открытое пространство путь свободен.
        let outside = Vec3::new(-5.0, 1.0, -5.0);
        assert!(ctx.is_unobstructed(outside, outside + Vec3::new(1.0, 0.0, 0.0)));
        let free = ctx.unobstructed_distance(Vec3::new(2.5, 1.0, 2.5), Vec3::new(3.0, 1.0, 2.5));
        assert!((free - 0.5).abs() < 1e-4);
        // Без подключённой коллизии препятствий нет.
        let blind = HookContext::new(&world);
        assert!(blind.is_unobstructed(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0)));
        // Нулевая длина не делит на ноль.
        assert_eq!(ctx.unobstructed_distance(Vec3::ONE, Vec3::ONE), 0.0);
    }

    #[test]
    fn modules_see_player_positions() {
        let mut h = harness();
        let a = h.join("a");
        let me = h.client(a).net_id().unwrap();
        let packet = h.client(a).command_packet("where", vec![]).unwrap();
        h.send(a, packet);
        // Точка появления в тестовом стенде: x = 2.5.
        assert_eq!(
            h.client(a).component(me, "test.count"),
            Some(&2u32.to_le_bytes()[..])
        );
    }

    #[test]
    fn invalid_commands_are_rejected_by_the_protocol_and_unknown_ones_ignored() {
        let mut h = harness();
        let a = h.join("a");
        assert!(h.client(a).command_packet("", vec![]).is_none());
        assert!(
            h.client(a)
                .command_packet(&"k".repeat(65), vec![])
                .is_none()
        );
        assert!(
            h.client(a)
                .command_packet("ok", vec![0; astrocore_protocol::MAX_COMPONENT_BYTES + 1])
                .is_none()
        );
        let me = h.client(a).net_id().unwrap();
        let packet = h
            .client(a)
            .command_packet("nobody.handles.this", vec![1])
            .unwrap();
        h.send(a, packet);
        assert_eq!(h.client(a).component(me, "test.count"), None);
        assert_eq!(h.server.player_count(), 1, "игрок не отключён");
    }
}
