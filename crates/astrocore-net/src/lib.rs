//! Сеть Astrocore по образцу игровых состояний Robust: сервер передаёт клиенту состояния
//! компонентов видимых ему сущностей (PVS), клиент подтверждает применённые тики.
//!
//! Крейт не знает о транспорте: сервер и клиент обмениваются пакетами через ресурсы
//! [`ServerNet`] и [`ClientNet`], а доставляет их `astrocore-transport` или тест.

mod appearance;
mod client;
mod components;
mod events;
mod ids;
mod protocol;
mod pvs;
mod registry;
mod remote_console;
mod server;
mod sync;
mod visibility;

use astrocore_console::ConsolePlugin;
use astrocore_ecs::{GamePlugin, GameWorldExt, MetaData, PreUpdate, RegistryWorldExt, Sync};
use astrocore_map::{ContainerManager, Map, MapGrid, MapPlugin, Transform};
use bevy_ecs::schedule::{IntoScheduleConfigs, Schedules};
use bevy_ecs::world::World;

pub use appearance::{Appearance, AppearanceChange, AppearanceValue, AppearanceWorldExt};
pub use client::{
    ClientNet, ClientStatus, ClientTransport, ClientWorldExt, Detached, LocalPlayerAttached,
    StateApplied, StateApplying, apply_state,
};
pub use events::{NetEvent, NetEventWorldExt, NetEvents, NetTarget};
pub use ids::{DeletionLog, NetEntities, NetEntity, NetSync};
pub use protocol::{
    ClientMessage, EntityState, GameState, MAX_STATE_PARTS, MAX_UNRELIABLE_BYTES, STATE_PART_BYTES,
    ServerMessage,
};
pub use pvs::{PVS_CHUNK_SIZE, PvsCell, PvsIndex, PvsOverrides, View, Visible, collect_visible};
pub use registry::{
    NetComponent, NetComponentInfo, NetError, NetRegistry, NetRegistryWorldExt, WriteContext,
};
pub use remote_console::{
    CONSOLE_LOG_LINES, ConsoleLine, ConsoleLog, RemoteCommand, RemoteCommandOutput,
    RemoteConsoleWorldExt,
};
pub use server::{
    Actor, ConnId, Incoming, NetUserId, Outgoing, PVS_ENTITY_BUDGET, PVS_RANGE, PlayerAttached,
    PlayerConnected, PlayerConnecting, PlayerDetached, PlayerDisconnected, ServerNet, Session,
    SessionSync, SessionWorldExt, complete_connection, reject_connection,
};
pub use sync::NetSet;
pub use visibility::{
    DEFAULT_VISIBILITY, Eye, Visibility, VisibilityLayers, VisibilityMask, VisibilityWorldExt,
    register_visibility_layer,
};

/// Система приёма пакетов сервера (этап `PreUpdate`): относительно неё упорядочиваются
/// системы, которым нужен свежий ввод игроков.
pub use server::receive as server_receive;

/// Система приёма пакетов клиента (этап `PreUpdate`): относительно неё упорядочиваются
/// системы, которым нужно свежее состояние сервера.
pub use client::receive as client_receive;

/// Сетевые компоненты движка: одинаковый набор у сервера и клиента.
fn register_engine_components(world: &mut World) {
    world.add_plugin(MapPlugin);
    world.add_plugin(ConsolePlugin);
    world.register_net_component::<MetaData>();
    world.register_net_component::<Transform>();
    world.register_net_component::<Map>();
    world.register_net_component::<MapGrid>();
    world.register_net_component::<ContainerManager>();
    world.register_data_component::<Appearance>();
    world.register_net_component::<Appearance>();
    visibility::register(world);
    world.init_resource::<NetEvents>();
    remote_console::register(world);
    world
        .resource_mut::<Schedules>()
        .configure_sets(Sync, (NetSet::Assign, NetSet::Mark, NetSet::Send).chain());
}

/// Сеть сервера: сессии, PVS и отправка состояний.
pub struct NetServerPlugin;

impl GamePlugin for NetServerPlugin {
    fn build(&self, world: &mut World) {
        register_engine_components(world);
        pvs::register(world);
        server::register(world);
        remote_console::register_server(world);
        world.add_game_systems(PreUpdate, server::receive);
        world.add_game_systems(Sync, server::assign_net_entities.in_set(NetSet::Assign));
        world.add_game_systems(Sync, server::send.in_set(NetSet::Send));
    }
}

/// Сеть клиента: приём и применение состояний.
pub struct NetClientPlugin;

impl GamePlugin for NetClientPlugin {
    fn build(&self, world: &mut World) {
        register_engine_components(world);
        client::register(world);
        remote_console::register_client(world);
        world.add_game_systems(PreUpdate, client::receive);
    }
}

#[cfg(test)]
mod tests;
