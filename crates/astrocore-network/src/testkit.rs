use std::collections::BTreeMap;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{GridCell, GridEdge};
use astrocore_protocol::ServerMessage;
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, StructureElement, StructureKind, World,
    WorldCommand, WorldConfig,
};

use crate::client::{ClientCore, ClientEvent};
use crate::server::{
    AllowAll, Authenticator, ConnId, GameServer, Out, ServerNetConfig, encode_out,
};

/// Идентификатор двери в [`test_world`].
pub const DOOR_ID: ObjectId = ObjectId::from_u128(9_000);

pub fn material() -> PrototypeId {
    PrototypeId::new("test:concrete").expect("валидный ID")
}

/// Два помещения 8×8 на одном этаже, соединённые дверью:
/// левое — x ∈ [0, 8), правое — x ∈ [8, 16); пол во всех ячейках.
pub fn test_world() -> World {
    let mut w = World::new("testkit", WorldConfig::default()).expect("конфигурация верна");
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "ground".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .expect("этаж");
    let mut id = 1u128;
    let mut put = |w: &mut World, kind, placement| {
        w.apply(WorldCommand::PlaceElement(StructureElement {
            id: ObjectId::from_u128(id),
            kind,
            placement,
            floor: FloorId(0),
            material: material(),
            thickness: 0.2,
        }))
        .expect("элемент");
        id += 1;
    };
    for x in 0..16 {
        for z in 0..8 {
            put(
                &mut w,
                StructureKind::Floor,
                Placement::Cell(GridCell::new(x, z)),
            );
        }
    }
    for x in 0..16 {
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(x, 0)),
        );
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_x(x, 8)),
        );
    }
    for z in 0..8 {
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(0, z)),
        );
        put(
            &mut w,
            StructureKind::Wall,
            Placement::Edge(GridEdge::along_z(16, z)),
        );
        if z != 3 {
            put(
                &mut w,
                StructureKind::Wall,
                Placement::Edge(GridEdge::along_z(8, z)),
            );
        }
    }
    w.apply(WorldCommand::PlaceElement(StructureElement {
        id: DOOR_ID,
        kind: StructureKind::Opening(OpeningKind::Door),
        placement: Placement::Edge(GridEdge::along_z(8, 3)),
        floor: FloorId(0),
        material: PrototypeId::new("test:door").expect("валидный ID"),
        thickness: 0.2,
    }))
    .expect("дверь");
    w.take_dirty();
    w
}

/// Аутентификатор с заданными правами по имени сборки: `edit` — редактор, иначе игрок.
pub struct ByBuild;

impl Authenticator for ByBuild {
    fn authenticate(
        &mut self,
        build: &str,
    ) -> Result<crate::server::Identity, astrocore_protocol::RejectReason> {
        let mut id = AllowAll {
            can_edit: build.starts_with("edit"),
        }
        .authenticate(build)?;
        if build.contains("keycard") {
            id.access.insert("keycard".into());
        }
        Ok(id)
    }
}

/// Фильтр потерь: возвращает `true` для сообщения, которое нужно «потерять».
pub type DropFilter = Box<dyn FnMut(ConnId, &ServerMessage) -> bool>;

/// Стенд: сервер и клиенты, пакеты передаются напрямую (с возможностью потери).
pub struct Harness {
    pub server: GameServer,
    pub clients: BTreeMap<ConnId, ClientCore>,
    pub events: BTreeMap<ConnId, Vec<ClientEvent>>,
    pub disconnected: Vec<ConnId>,
    /// Сообщения, которые стенд «теряет» (для проверки восстановления).
    pub drop_filter: Option<DropFilter>,
    next: u64,
}

impl Harness {
    pub fn new(cfg: ServerNetConfig) -> Self {
        Self::with_world(test_world(), cfg)
    }

    pub fn with_world(world: World, cfg: ServerNetConfig) -> Self {
        Self::with_server(GameServer::new(world, cfg, Box::new(ByBuild)))
    }

    /// Стенд вокруг готового сервера (например, запущенного редактором для пробной игры).
    pub fn with_server(server: GameServer) -> Self {
        Self {
            server,
            clients: BTreeMap::new(),
            events: BTreeMap::new(),
            disconnected: Vec::new(),
            drop_filter: None,
            next: 1,
        }
    }

    pub fn default_cfg() -> ServerNetConfig {
        ServerNetConfig {
            spawn: astrocore_math::glam::Vec3::new(2.5, 1.0, 2.5),
            ..ServerNetConfig::default()
        }
    }

    /// Подключить клиента и довести рукопожатие до конца.
    pub fn join(&mut self, build: &str) -> ConnId {
        let id = ConnId(self.next);
        self.next += 1;
        let hash = self.server.world().content_hash();
        let client = ClientCore::new(build, self.server.world().id(), hash);
        let hello = client.hello_packet();
        self.clients.insert(id, client);
        let outs = self.server.connect(id);
        self.deliver(outs);
        let outs = self.server.on_packet(id, &hello);
        self.deliver(outs);
        id
    }

    pub fn deliver(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Send { to, msg } => {
                    if let Some(f) = self.drop_filter.as_mut()
                        && f(to, &msg)
                    {
                        continue;
                    }
                    let bytes = encode_out(&msg).expect("сервер кодирует корректно");
                    if let Some(c) = self.clients.get_mut(&to) {
                        let evs = c.on_packet(&bytes).expect("клиент принимает пакет сервера");
                        self.events.entry(to).or_default().extend(evs);
                    }
                }
                Out::Disconnect(c) => {
                    self.disconnected.push(c);
                    let more = self.server.disconnect(c);
                    self.clients.remove(&c);
                    self.deliver(more);
                }
            }
        }
    }

    /// Пакеты клиента → сервер, затем тик сервера.
    pub fn send(&mut self, from: ConnId, packet: Vec<u8>) {
        let outs = self.server.on_packet(from, &packet);
        self.deliver(outs);
    }

    pub fn tick(&mut self) {
        let outs = self.server.tick();
        self.deliver(outs);
    }

    pub fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// Один тик ввода клиента: предсказание + отправка + тик сервера.
    pub fn input(&mut self, who: ConnId, move_x: i8, move_z: i8, yaw: f32) {
        if let Some(p) = self
            .clients
            .get_mut(&who)
            .and_then(|c| c.input_packet(move_x, move_z, yaw))
        {
            self.send(who, p);
        }
        self.tick();
    }

    /// Включить у клиента предсказание полёта призрака.
    pub fn client_noclip(&mut self, id: ConnId, on: bool) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.set_noclip(on);
        }
    }

    pub fn client(&self, id: ConnId) -> &ClientCore {
        &self.clients[&id]
    }

    pub fn take_events(&mut self, id: ConnId) -> Vec<ClientEvent> {
        self.events.remove(&id).unwrap_or_default()
    }
}

// ---- имитация сети для тестов UDP ----------------------------------------------------

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;

use crate::udp::Datagram;

struct NetInner {
    now: u64,
    rng: u64,
    loss: u64,
    dup: u64,
    min_delay: u64,
    max_delay: u64,
    partitioned: bool,
    queues: HashMap<SocketAddr, Vec<(u64, Vec<u8>, SocketAddr)>>,
    wire: Vec<Vec<u8>>,
}

impl NetInner {
    fn rand(&mut self) -> u64 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.rng >> 33
    }
}

/// Сеть с потерями, дублями, задержками и перестановкой. Время задаёт тест.
#[derive(Clone)]
pub struct SimNet {
    inner: Rc<RefCell<NetInner>>,
}

impl SimNet {
    pub fn new(seed: u64, loss_percent: u64, dup_percent: u64, delay_ms: (u64, u64)) -> Self {
        Self {
            inner: Rc::new(RefCell::new(NetInner {
                now: 0,
                rng: seed,
                loss: loss_percent,
                dup: dup_percent,
                min_delay: delay_ms.0,
                max_delay: delay_ms.1.max(delay_ms.0),
                partitioned: false,
                queues: HashMap::new(),
                wire: Vec::new(),
            })),
        }
    }

    pub fn set_time(&self, now_ms: u64) {
        self.inner.borrow_mut().now = now_ms;
    }

    /// Разрыв связи: все датаграммы теряются, пока включено.
    pub fn set_partitioned(&self, on: bool) {
        self.inner.borrow_mut().partitioned = on;
    }

    pub fn socket(&self, addr: SocketAddr) -> SimSocket {
        SimSocket {
            net: self.clone(),
            addr,
        }
    }

    /// Всё, что «видел» наблюдатель на линии (для проверки отсутствия открытого текста).
    pub fn wire(&self) -> Vec<Vec<u8>> {
        self.inner.borrow().wire.clone()
    }

    /// Вбросить датаграмму от имени `from` адресату `to` (атакующий).
    pub fn inject(&self, from: SocketAddr, to: SocketAddr, data: Vec<u8>) {
        let mut n = self.inner.borrow_mut();
        let at = n.now;
        n.queues.entry(to).or_default().push((at, data, from));
    }
}

pub struct SimSocket {
    net: SimNet,
    addr: SocketAddr,
}

impl Datagram for SimSocket {
    fn send_to(&mut self, data: &[u8], to: SocketAddr) -> std::io::Result<()> {
        let mut n = self.net.inner.borrow_mut();
        n.wire.push(data.to_vec());
        if n.partitioned || n.rand() % 100 < n.loss {
            return Ok(());
        }
        let span = n.max_delay - n.min_delay + 1;
        let at = n.now + n.min_delay + n.rand() % span;
        n.queues
            .entry(to)
            .or_default()
            .push((at, data.to_vec(), self.addr));
        if n.rand() % 100 < n.dup {
            let extra = n.rand() % 30;
            n.queues
                .entry(to)
                .or_default()
                .push((at + extra, data.to_vec(), self.addr));
        }
        Ok(())
    }

    fn recv_from(&mut self) -> std::io::Result<Option<(Vec<u8>, SocketAddr)>> {
        let mut n = self.net.inner.borrow_mut();
        let now = n.now;
        let q = n.queues.entry(self.addr).or_default();
        let best = q
            .iter()
            .enumerate()
            .filter(|(_, (t, _, _))| *t <= now)
            .min_by_key(|(_, (t, _, _))| *t)
            .map(|(i, _)| i);
        Ok(best.map(|i| {
            let (_, data, from) = q.swap_remove(i);
            (data, from)
        }))
    }
}
