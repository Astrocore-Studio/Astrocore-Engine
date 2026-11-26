//! Связь транспорта с сетью игры: пакеты Lightyear ↔ почтовые ящики [`ServerNet`] и
//! [`ClientNet`] из `astrocore-net`.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use astrocore_net::{ClientNet, ConnId, Incoming, ServerNet};
use bevy_ecs::world::World;

use crate::{ConnKey, LyClient, LyServer};

/// Версия сетевого протокола игры на ядре сущностей: клиент другой версии не подключится.
pub const GAME_PROTOCOL_ID: u64 = 0x4153_5452_4f45_0001;

/// Транспорт сервера игры.
pub struct ServerLink {
    transport: LyServer,
    live: BTreeSet<ConnKey>,
}

impl ServerLink {
    /// Начинает слушать адрес. `key` — секрет соединений, общий с клиентами.
    pub fn start(addr: SocketAddr, key: [u8; 32], tick_hz: u32) -> Self {
        Self {
            transport: LyServer::start_with_protocol(addr, key, tick_hz, GAME_PROTOCOL_ID),
            live: BTreeSet::new(),
        }
    }

    /// Адрес, на котором слушает сервер.
    pub fn local_addr(&self) -> SocketAddr {
        self.transport.local_addr()
    }

    /// Принимает пакеты и разрывы соединений в мир. Вызывается перед тактом мира.
    pub fn receive(&mut self, world: &mut World) {
        self.transport.step();
        let live: BTreeSet<ConnKey> = self.transport.connections().into_iter().collect();
        let mut net = world.resource_mut::<ServerNet>();
        for gone in self.live.difference(&live) {
            net.incoming
                .push(Incoming::Disconnected(ConnId(u64::from(gone.0))));
        }
        self.live = live;
        for (conn, bytes) in self.transport.take_packets() {
            net.incoming
                .push(Incoming::Packet(ConnId(u64::from(conn.0)), bytes));
        }
    }

    /// Отправляет пакеты мира клиентам. Вызывается после такта мира.
    pub fn send(&mut self, world: &mut World) {
        let outgoing = std::mem::take(&mut world.resource_mut::<ServerNet>().outgoing);
        for packet in outgoing {
            self.transport
                .send(ConnKey(packet.conn.0 as u32), packet.reliable, packet.bytes);
        }
        self.transport.step();
    }
}

/// Транспорт клиента игры.
pub struct ClientLink {
    transport: LyClient,
}

impl ClientLink {
    /// Начинает соединение с сервером.
    pub fn connect(
        server: SocketAddr,
        client_id: u64,
        key: [u8; 32],
        tick_hz: u32,
    ) -> Result<Self, String> {
        Ok(Self {
            transport: LyClient::connect_with_protocol(
                server,
                client_id,
                key,
                tick_hz,
                GAME_PROTOCOL_ID,
            )?,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.transport.is_connected()
    }

    /// Принимает пакеты сервера в мир. Вызывается перед тактом мира.
    pub fn receive(&mut self, world: &mut World) {
        self.transport.step();
        let packets = self.transport.take_packets();
        world.resource_mut::<ClientNet>().incoming.extend(packets);
    }

    /// Отправляет пакеты мира серверу. Вызывается после такта мира.
    pub fn send(&mut self, world: &mut World) {
        let outgoing = std::mem::take(&mut world.resource_mut::<ClientNet>().outgoing);
        for (reliable, bytes) in outgoing {
            self.transport.send(reliable, bytes);
        }
        self.transport.step();
    }
}
