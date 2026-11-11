mod host;

pub use host::LyGameHost;

use std::net::SocketAddr;
use std::time::Duration;

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::server::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

/// Пакет протокола Astrocore как сообщение Lightyear: полезная нагрузка — готовые байты
/// `astrocore_protocol`, их разбор и проверку выполняет движок, а не транспорт.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstroPacket(pub Vec<u8>);

/// Надёжный упорядоченный канал: вход в игру, структурные операции, двери, содержимое.
pub struct ReliableChannel;

/// Последовательный ненадёжный канал: движение и частые обновления.
pub struct StateChannel;

/// Идентификатор протокола в рукопожатии netcode: клиент другой версии не подключится.
pub const PROTOCOL_ID: u64 = 0x4153_5452_4f43_0001;

/// Принятые за кадр пакеты. Lightyear очищает приёмники в конце кадра, поэтому пакеты
/// переносятся в ресурс системой внутри кадра, а хозяин забирает их после `update`.
#[derive(Resource, Default)]
struct Inbox(Vec<(Entity, Vec<u8>)>);

fn drain_receivers(
    mut inbox: ResMut<Inbox>,
    mut q: Query<(Entity, &mut MessageReceiver<AstroPacket>)>,
) {
    for (e, mut rx) in &mut q {
        inbox.0.extend(rx.receive().map(|p| (e, p.0)));
    }
}

/// Общая регистрация сообщений и каналов — одинаковая у клиента и сервера.
pub struct AstroProtocolPlugin;

impl Plugin for AstroProtocolPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Inbox>()
            .add_systems(Update, drain_receivers);
        app.register_message::<AstroPacket>()
            .add_direction(NetworkDirection::Bidirectional);
        app.add_channel::<ReliableChannel>(ChannelSettings {
            mode: ChannelMode::OrderedReliable(ReliableSettings::default()),
            ..default()
        })
        .add_direction(NetworkDirection::Bidirectional);
        app.add_channel::<StateChannel>(ChannelSettings {
            mode: ChannelMode::SequencedUnreliable,
            ..default()
        })
        .add_direction(NetworkDirection::Bidirectional);
    }
}

/// Номер соединения на стороне сервера (индекс сущности соединения Lightyear).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnKey(pub u32);

fn tick_duration(hz: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(hz.max(1)))
}

/// Сервер на Lightyear (UDP + netcode). Хозяин вызывает `step` в своём цикле и передаёт
/// принятые пакеты в `GameServer`, а ответы — в `send`.
pub struct LyServer {
    app: App,
    server: Entity,
    local: SocketAddr,
}

impl LyServer {
    /// `key` — 32-байтовый секрет netcode (токены подключения); `tick_hz` — частота тика Lightyear.
    pub fn start(addr: SocketAddr, key: [u8; 32], tick_hz: u32) -> Self {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            ServerPlugins {
                tick_duration: tick_duration(tick_hz),
            },
            AstroProtocolPlugin,
        ));
        let server = app
            .world_mut()
            .spawn((
                NetcodeServer::new(
                    lightyear::netcode::server_plugin::NetcodeConfig::default()
                        .with_protocol_id(PROTOCOL_ID)
                        .with_key(key),
                ),
                LocalAddr(addr),
                ServerUdpIo,
            ))
            .id();
        app.finish();
        app.cleanup();
        app.world_mut().trigger(Start { entity: server });
        app.update();
        let local = app.world().get::<LocalAddr>(server).map_or(addr, |a| a.0);
        Self { app, server, local }
    }

    /// Адрес, на котором слушает сервер (с реальным портом, если запрошен 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Один кадр Lightyear: сетевой ввод-вывод, рукопожатия, доставка.
    pub fn step(&mut self) {
        self.app.update();
    }

    pub fn connections(&mut self) -> Vec<ConnKey> {
        let world = self.app.world_mut();
        let mut q = world.query_filtered::<Entity, (With<LinkOf>, With<Connected>)>();
        let mut v: Vec<ConnKey> = q.iter(world).map(|e| ConnKey(e.index_u32())).collect();
        v.sort();
        v
    }

    /// Принятые пакеты протокола с момента прошлого вызова.
    pub fn take_packets(&mut self) -> Vec<(ConnKey, Vec<u8>)> {
        let mut inbox = self.app.world_mut().resource_mut::<Inbox>();
        std::mem::take(&mut inbox.0)
            .into_iter()
            .map(|(e, b)| (ConnKey(e.index_u32()), b))
            .collect()
    }

    /// Отправить пакет клиенту: `reliable` — по надёжному каналу, иначе по последовательному.
    pub fn send(&mut self, to: ConnKey, reliable: bool, bytes: Vec<u8>) -> bool {
        let world = self.app.world_mut();
        let mut q = world.query::<(Entity, &mut MessageSender<AstroPacket>)>();
        for (e, mut tx) in q.iter_mut(world) {
            if e.index_u32() == to.0 {
                if reliable {
                    tx.send::<ReliableChannel>(AstroPacket(bytes));
                } else {
                    tx.send::<StateChannel>(AstroPacket(bytes));
                }
                return true;
            }
        }
        false
    }

    pub fn server_entity(&self) -> Entity {
        self.server
    }
}

/// Клиент на Lightyear.
pub struct LyClient {
    app: App,
    client: Entity,
    /// Пакеты, отправленные до установки соединения: уходят при первом кадре с `Connected`.
    queued: Vec<(bool, Vec<u8>)>,
}

impl LyClient {
    pub fn connect(
        server: SocketAddr,
        client_id: u64,
        key: [u8; 32],
        tick_hz: u32,
    ) -> Result<Self, String> {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            ClientPlugins {
                tick_duration: tick_duration(tick_hz),
            },
            AstroProtocolPlugin,
        ));
        let auth = Authentication::Manual {
            server_addr: server,
            client_id,
            private_key: key,
            protocol_id: PROTOCOL_ID,
        };
        let netcode = NetcodeClient::new(
            auth,
            lightyear::netcode::client_plugin::NetcodeConfig::default(),
        )
        .map_err(|e| e.to_string())?;
        let client = app
            .world_mut()
            .spawn((
                netcode,
                UdpIo::default(),
                LocalAddr(SocketAddr::from(([127, 0, 0, 1], 0))),
                PeerAddr(server),
            ))
            .id();
        app.finish();
        app.cleanup();
        app.world_mut().trigger(Connect { entity: client });
        app.update();
        Ok(Self {
            app,
            client,
            queued: Vec::new(),
        })
    }

    pub fn step(&mut self) {
        self.app.update();
        if self.is_connected() && !self.queued.is_empty() {
            for (reliable, bytes) in std::mem::take(&mut self.queued) {
                self.send_now(reliable, bytes);
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.app.world().get::<Connected>(self.client).is_some()
    }

    pub fn take_packets(&mut self) -> Vec<Vec<u8>> {
        let mut inbox = self.app.world_mut().resource_mut::<Inbox>();
        std::mem::take(&mut inbox.0)
            .into_iter()
            .map(|(_, b)| b)
            .collect()
    }

    /// Отправить пакет; до подключения пакеты ждут в очереди (до 256 штук).
    pub fn send(&mut self, reliable: bool, bytes: Vec<u8>) -> bool {
        if self.is_connected() {
            return self.send_now(reliable, bytes);
        }
        if self.queued.len() >= 256 {
            return false;
        }
        self.queued.push((reliable, bytes));
        true
    }

    fn send_now(&mut self, reliable: bool, bytes: Vec<u8>) -> bool {
        let Some(mut tx) = self
            .app
            .world_mut()
            .get_mut::<MessageSender<AstroPacket>>(self.client)
        else {
            return false;
        };
        if reliable {
            tx.send::<ReliableChannel>(AstroPacket(bytes));
        } else {
            tx.send::<StateChannel>(AstroPacket(bytes));
        }
        true
    }
}
