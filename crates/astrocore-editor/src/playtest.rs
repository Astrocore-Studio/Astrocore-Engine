use std::io;

use astrocore_math::GridCell;
use astrocore_math::glam::Vec3;
use astrocore_network::server::{AllowAll, GameServer, ServerNetConfig};
use astrocore_network::tcp::TcpServer;
use astrocore_world::{FloorId, World};

#[derive(Debug, Clone, Copy)]
pub struct PlaytestConfig {
    pub floor: FloorId,
    /// Ячейка появления; игрок возникает на метр выше пола этажа и «встаёт» на поверхность.
    pub spawn_cell: GridCell,
    pub tick_hz: u8,
}

fn server_config(world: &World, cfg: &PlaytestConfig) -> Option<ServerNetConfig> {
    let floor = world.floor(cfg.floor)?;
    let step = world.config().grid_step_m;
    let center = cfg.spawn_cell.min_corner(step, floor.base_y + 1.0)
        + Vec3::new(step * 0.5, 0.0, step * 0.5);
    Some(ServerNetConfig {
        spawn: center,
        tick_hz: cfg.tick_hz,
        ..ServerNetConfig::default()
    })
}

/// Сервер для пробной игры на копии карты. Все игроки могут править мир.
pub fn start_playtest(world: &World, cfg: &PlaytestConfig) -> Option<GameServer> {
    let net = server_config(world, cfg)?;
    Some(GameServer::new(
        world.clone(),
        net,
        Box::new(AllowAll { can_edit: true }),
    ))
}

/// То же, но с TCP-слушателем на локальном адресе (порт выбирает система).
pub fn start_playtest_tcp(world: &World, cfg: &PlaytestConfig) -> io::Result<TcpServer> {
    let server = start_playtest(world, cfg)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "неизвестный этаж"))?;
    TcpServer::bind("127.0.0.1:0", server)
}
