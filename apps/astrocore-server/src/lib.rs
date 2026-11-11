//! Headless-сервер Astrocore.
//!
//! Не создаёт окно, графический контекст и не загружает текстуры (принцип 3
//! раздела 3). Состояние мира сохраняется как «снимок + журнал»
//! (`astrocore-persistence`): каждая подтверждённая операция попадает в журнал
//! на стадии `Replication`, контрольная точка пишется периодически и при выходе.

use std::path::PathBuf;
use std::time::Duration;

use astrocore_bevy::{CanonicalWorld, CommandQueue, OpOutbox, SimSet, SimulationPlugin};
use astrocore_persistence::{PersistError, Store};
use astrocore_world::{FloorDef, FloorId, World, WorldCommand, WorldConfig, WorldError};
use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

pub const DEFAULT_TICK_HZ: f64 = 20.0;
/// Через сколько записанных операций писать контрольную точку.
pub const SNAPSHOT_EVERY_OPS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Persist(#[from] PersistError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error("скрипт команд: {0}")]
    Script(String),
}

/// Настройки запуска.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub world_id: String,
    pub tick_hz: f64,
    /// Сколько тиков выполнить и выйти. `None` — работать до остановки процесса.
    pub max_ticks: Option<u64>,
    /// JSON-массив `WorldCommand`, который ставится в очередь при старте.
    pub script: Option<PathBuf>,
    /// Адрес TCP-слушателя для игроков. `None` — режим без сети (пакетные задачи).
    pub listen: Option<String>,
    /// Разрешить подключённым игрокам править структуру мира (для пробной игры и разработки).
    pub allow_edit: bool,
    /// Точка появления игроков.
    pub spawn: Option<[f32; 3]>,
}

impl ServerConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            world_id: "world".into(),
            tick_hz: DEFAULT_TICK_HZ,
            max_ticks: None,
            script: None,
            listen: None,
            allow_edit: false,
            spawn: None,
        }
    }
}

/// Итог работы сервера (для тестов и логов).
#[derive(Debug)]
pub struct RunReport {
    pub ticks: u64,
    pub revision: u64,
    pub element_count: usize,
    pub content_hash: u64,
    pub rejected: usize,
    pub recovered: bool,
}

#[derive(Resource)]
struct Persistence {
    store: Store,
    ops_since_snapshot: usize,
    failed: Option<String>,
}

#[derive(Resource, Default)]
struct RejectedTotal(usize);

/// Загрузить сохранённый мир или создать новый с этажом `ground`.
pub fn load_or_create(store: &Store, world_id: &str) -> Result<(World, bool), ServerError> {
    if let Some(recovered) = store.recover()? {
        return Ok((recovered.world, true));
    }
    let mut world = World::new(world_id, WorldConfig::default())?;
    world.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "ground".into(),
        base_y: 0.0,
        height: 3.0,
    }))?;
    store.save_snapshot(&world)?;
    Ok((world, false))
}

/// Сетевой сервер: настоящий игровой сервер (`GameServer`) на TCP с журналом и снимками.
pub struct ListenServer {
    tcp: astrocore_network::tcp::TcpServer,
    store: Store,
    ticks: u64,
    ops_since_snapshot: usize,
}

impl ListenServer {
    pub fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        use astrocore_network::server::{AllowAll, GameServer, ServerNetConfig};
        let store = Store::open(&config.data_dir)?;
        let (mut world, _) = load_or_create(&store, &config.world_id)?;
        if let Some(path) = &config.script {
            let text =
                std::fs::read_to_string(path).map_err(|e| ServerError::Script(e.to_string()))?;
            let commands: Vec<WorldCommand> =
                serde_json::from_str(&text).map_err(|e| ServerError::Script(e.to_string()))?;
            for c in commands {
                // Недопустимые команды скрипта не фатальны (как и в пакетном режиме).
                if let Ok(op) = world.apply(c) {
                    store.append(&op)?;
                }
            }
        }
        let mut net = ServerNetConfig {
            tick_hz: config.tick_hz.round().clamp(1.0, 240.0) as u8,
            ..ServerNetConfig::default()
        };
        if let Some(s) = config.spawn {
            net.spawn = astrocore_math::glam::Vec3::from_array(s);
        }
        let core = GameServer::new(
            world,
            net,
            Box::new(AllowAll {
                can_edit: config.allow_edit,
            }),
        );
        let addr = config.listen.as_deref().unwrap_or("127.0.0.1:0");
        let tcp = astrocore_network::tcp::TcpServer::bind(addr, core)
            .map_err(|e| ServerError::Script(format!("слушатель {addr}: {e}")))?;
        Ok(Self {
            tcp,
            store,
            ticks: 0,
            ops_since_snapshot: 0,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.tcp.local_addr()
    }

    pub fn core(&self) -> &astrocore_network::server::GameServer {
        self.tcp.core()
    }

    /// Один шаг: сеть + тик + запись операций в журнал.
    pub fn step(&mut self) -> Result<(), ServerError> {
        self.tcp
            .step()
            .map_err(|e| ServerError::Script(e.to_string()))?;
        self.ticks += 1;
        for op in self.tcp.core_mut().take_applied_ops() {
            self.store.append(&op)?;
            self.ops_since_snapshot += 1;
        }
        if self.ops_since_snapshot >= SNAPSHOT_EVERY_OPS {
            self.store.save_snapshot(self.tcp.core().world())?;
            self.ops_since_snapshot = 0;
        }
        Ok(())
    }

    /// Работать с частотой тика до `max_ticks` (или бесконечно), затем сохранить мир.
    pub fn run(&mut self, tick_hz: f64, max_ticks: Option<u64>) -> Result<RunReport, ServerError> {
        let period = Duration::from_secs_f64(1.0 / tick_hz);
        let mut next = std::time::Instant::now();
        let mut done = 0u64;
        let mut last_log = std::time::Instant::now();
        while max_ticks.is_none_or(|m| done < m) {
            self.step()?;
            done += 1;
            if last_log.elapsed() >= Duration::from_secs(30) {
                let m = self.core().metrics();
                eprintln!(
                    "метрики: тиков {}, игроков {}, пакетов {}, правок {}, отказов {}",
                    m.ticks,
                    self.core().player_count(),
                    m.packets_in,
                    m.edits_applied,
                    m.rejected_packets
                );
                last_log = std::time::Instant::now();
            }
            next += period;
            match next.checked_duration_since(std::time::Instant::now()) {
                Some(wait) => std::thread::sleep(wait),
                None => next = std::time::Instant::now(),
            }
        }
        self.finish()
    }

    /// Сохранить финальный снимок и вернуть отчёт.
    pub fn finish(&mut self) -> Result<RunReport, ServerError> {
        let world = self.tcp.core().world();
        self.store.save_snapshot(world)?;
        Ok(RunReport {
            ticks: self.ticks,
            revision: world.revision().0,
            element_count: world.element_count(),
            content_hash: world.content_hash(),
            rejected: self.tcp.core().metrics().rejected_packets as usize,
            recovered: false,
        })
    }
}

pub fn run(config: &ServerConfig) -> Result<RunReport, ServerError> {
    if config.listen.is_some() {
        return ListenServer::start(config)?.run(config.tick_hz, config.max_ticks);
    }
    let store = Store::open(&config.data_dir)?;
    let (world, recovered) = load_or_create(&store, &config.world_id)?;

    let mut app = App::new();
    match config.max_ticks {
        // Ограниченный запуск (тесты, пакетные задачи): время продвигается вручную,
        // один `update()` = один тик — результат не зависит от скорости машины.
        Some(_) => {
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / config.tick_hz,
                )));
        }
        None => {
            app.add_plugins(
                MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_millis(2))),
            );
        }
    }
    app.add_plugins(SimulationPlugin {
        world,
        tick_hz: config.tick_hz,
    })
    .insert_resource(Persistence {
        store: store.clone(),
        ops_since_snapshot: 0,
        failed: None,
    })
    .init_resource::<RejectedTotal>()
    .add_systems(FixedUpdate, persist_outbox.in_set(SimSet::Replication));

    if let Some(path) = &config.script {
        let text = std::fs::read_to_string(path).map_err(|e| ServerError::Script(e.to_string()))?;
        let commands: Vec<WorldCommand> =
            serde_json::from_str(&text).map_err(|e| ServerError::Script(e.to_string()))?;
        app.world_mut()
            .resource_mut::<CommandQueue>()
            .0
            .extend(commands);
    }

    let mut ticks = 0;
    match config.max_ticks {
        Some(max) => {
            app.update(); // инициализация часов: тиков ещё нет
            while ticks < max {
                app.update();
                ticks += 1;
            }
        }
        None => {
            app.run();
        }
    }

    if let Some(err) = app.world().resource::<Persistence>().failed.clone() {
        return Err(ServerError::Script(format!("сбой записи журнала: {err}")));
    }
    let world = &app.world().resource::<CanonicalWorld>().0;
    store.save_snapshot(world)?;
    Ok(RunReport {
        ticks,
        revision: world.revision().0,
        element_count: world.element_count(),
        content_hash: world.content_hash(),
        rejected: app.world().resource::<RejectedTotal>().0,
        recovered,
    })
}

/// Записать подтверждённые операции в журнал; изредка — контрольная точка.
fn persist_outbox(
    mut outbox: ResMut<OpOutbox>,
    mut persistence: ResMut<Persistence>,
    mut rejected: ResMut<RejectedTotal>,
    mut rejected_now: ResMut<astrocore_bevy::RejectedCommands>,
    world: Res<CanonicalWorld>,
) {
    rejected.0 += rejected_now.0.len();
    rejected_now.0.clear();
    for op in std::mem::take(&mut outbox.0) {
        if let Err(e) = persistence.store.append(&op) {
            persistence.failed.get_or_insert_with(|| e.to_string());
            return;
        }
        persistence.ops_since_snapshot += 1;
    }
    if persistence.ops_since_snapshot >= SNAPSHOT_EVERY_OPS {
        match persistence.store.save_snapshot(&world.0) {
            Ok(()) => persistence.ops_since_snapshot = 0,
            Err(e) => {
                persistence.failed.get_or_insert_with(|| e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use astrocore_core::{ObjectId, PrototypeId};
    use astrocore_math::GridEdge;
    use astrocore_world::{Placement, StructureElement, StructureKind};

    use super::*;

    fn wall(id: u128, x: i32) -> WorldCommand {
        WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind: StructureKind::Wall,
            placement: Placement::Edge(GridEdge::along_x(x, 0)),
            floor: FloorId(0),
            material: PrototypeId::new("t:m").unwrap(),
            thickness: 0.2,
        })
    }

    fn write_script(dir: &std::path::Path, cmds: &[WorldCommand]) -> PathBuf {
        let p = dir.join("script.json");
        fs::write(&p, serde_json::to_string(cmds).unwrap()).unwrap();
        p
    }

    #[test]
    fn first_run_creates_world_and_applies_script() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.max_ticks = Some(5);
        cfg.script = Some(write_script(dir.path(), &[wall(1, 0), wall(2, 1)]));
        let report = run(&cfg).unwrap();
        assert!(!report.recovered);
        assert_eq!(
            (report.ticks, report.element_count, report.rejected),
            (5, 2, 0)
        );
        assert_eq!(report.revision, 3, "этаж + две стены");
    }

    #[test]
    fn restart_restores_identical_world() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.max_ticks = Some(3);
        cfg.script = Some(write_script(
            dir.path(),
            &[
                wall(1, 0),
                wall(2, 1),
                WorldCommand::RemoveElement {
                    id: ObjectId::from_u128(1),
                },
            ],
        ));
        let first = run(&cfg).unwrap();

        cfg.script = None;
        let second = run(&cfg).unwrap();
        assert!(second.recovered);
        assert_eq!(
            second.content_hash, first.content_hash,
            "перезапуск воспроизводит то же состояние"
        );
        assert_eq!(second.element_count, 1);
    }

    #[test]
    fn crash_between_snapshots_is_recovered_from_journal() {
        // Первый запуск пишет журнал; имитируем аварийный выход, удалив финальный
        // снимок и вернув стартовый (мир только с этажом) — журнал должен догнать.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = Store::open(&data).unwrap();
        let (world0, _) = load_or_create(&store, "w").unwrap();
        let initial_snapshot = fs::read(data.join("snapshot.json")).unwrap();
        drop(world0);

        let mut cfg = ServerConfig::new(&data);
        cfg.max_ticks = Some(3);
        cfg.script = Some(write_script(dir.path(), &[wall(1, 0), wall(2, 1)]));
        let first = run(&cfg).unwrap();

        // Вместо финального снимка — стартовый + журнал, как после падения до записи снимка.
        let journal = {
            // run() очистил журнал при финальном снимке, поэтому воссоздаём его из команд.
            let mut w = World::from_state(serde_json::from_value(
                serde_json::from_slice::<serde_json::Value>(&initial_snapshot).unwrap()["world"].clone(),
            ).unwrap()).unwrap();
            let s = Store::open(&data).unwrap();
            fs::write(data.join("snapshot.json"), &initial_snapshot).unwrap();
            for c in [wall(1, 0), wall(2, 1)] {
                s.append(&w.apply(c).unwrap()).unwrap();
            }
            w.content_hash()
        };
        let mut cfg2 = ServerConfig::new(&data);
        cfg2.max_ticks = Some(1);
        let second = run(&cfg2).unwrap();
        assert!(second.recovered);
        assert_eq!(second.content_hash, journal);
        assert_eq!(second.content_hash, first.content_hash);
    }

    #[test]
    fn rejected_script_commands_are_counted_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.max_ticks = Some(2);
        cfg.script = Some(write_script(
            dir.path(),
            &[wall(1, 0), wall(2, 0), wall(1, 5)],
        ));
        let report = run(&cfg).unwrap();
        assert_eq!((report.element_count, report.rejected), (1, 2));
    }

    #[test]
    fn bad_script_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        fs::write(&p, "{ nope").unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.max_ticks = Some(1);
        cfg.script = Some(p);
        assert!(matches!(run(&cfg), Err(ServerError::Script(_))));
        cfg.script = Some(dir.path().join("missing.json"));
        assert!(matches!(run(&cfg), Err(ServerError::Script(_))));
    }

    #[test]
    fn network_server_serves_clients_persists_edits_and_restores_them() {
        use astrocore_network::client::ClientCore;
        use astrocore_network::tcp::TcpClient;

        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.world_id = "net-world".into();
        cfg.listen = Some("127.0.0.1:0".into());
        cfg.allow_edit = true;
        let mut server = ListenServer::start(&cfg).unwrap();
        let addr = server.local_addr().unwrap();

        let mut tcp = TcpClient::connect(addr).unwrap();
        let mut core = ClientCore::new("editor", "net-world", 0);
        tcp.send(&core.hello_packet()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut sent = false;
        while std::time::Instant::now() < deadline {
            server.step().unwrap();
            for p in tcp.poll() {
                core.on_packet(&p).unwrap();
            }
            if core.is_ready() && !sent {
                tcp.send(&core.edit_packet(wall(555, 2)).unwrap()).unwrap();
                sent = true;
            }
            if sent
                && core
                    .world()
                    .is_some_and(|w| w.element(ObjectId::from_u128(555)).is_some())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            core.world()
                .is_some_and(|w| w.element(ObjectId::from_u128(555)).is_some()),
            "правка дошла до клиента"
        );
        let report = server.finish().unwrap();
        assert_eq!(report.element_count, 1);
        drop(server);

        // Перезапуск без сети восстанавливает мир с правкой клиента.
        let mut again = ServerConfig::new(dir.path().join("data"));
        again.max_ticks = Some(1);
        let restored = run(&again).unwrap();
        assert!(restored.recovered);
        assert_eq!(restored.content_hash, report.content_hash);
    }

    #[test]
    fn periodic_snapshot_resets_journal_counter() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig::new(dir.path().join("data"));
        cfg.max_ticks = Some(2);
        let cmds: Vec<_> = (0..(SNAPSHOT_EVERY_OPS as i32 + 5))
            .map(|i| wall(1000 + i as u128, i))
            .collect();
        cfg.script = Some(write_script(dir.path(), &cmds));
        let report = run(&cfg).unwrap();
        assert_eq!(report.element_count, SNAPSHOT_EVERY_OPS + 5);
        let again = run(&ServerConfig {
            script: None,
            ..cfg
        })
        .unwrap();
        assert_eq!(again.content_hash, report.content_hash);
    }
}
