use std::path::PathBuf;
use std::process::ExitCode;

use astrocore_server::{ServerConfig, run};

const USAGE: &str = "\
astrocore-server — headless-сервер Astrocore

Использование:
  astrocore-server --data-dir <каталог> [--world-id <id>] [--tick-hz <гц>]
                   [--ticks <N>] [--script <команды.json>]
                   [--listen <адрес:порт>] [--listen-udp <адрес:порт> --key-file <файл>]
                   [--allow-edit] [--spawn x,y,z]

  --data-dir   каталог сохранений (снимок + журнал); создаётся при отсутствии
  --world-id   идентификатор нового мира (по умолчанию world)
  --tick-hz    частота фиксированного шага, Гц (по умолчанию 20)
  --ticks      выполнить N тиков и выйти (по умолчанию — работать до остановки)
  --script     JSON-массив WorldCommand для постановки в очередь при старте
  --listen     принимать игроков по TCP на этом адресе (например 127.0.0.1:7777)
  --listen-udp принимать игроков по шифрованному UDP; печатает открытый ключ сервера
  --key-file   файл долговременного ключа сервера (создаётся при отсутствии, права 600)
  --listen-lightyear  принимать игроков через Lightyear (UDP + netcode); нужен --ly-key <hex32>
  --backup-keep N  после каждого снимка делать резервную копию (хранить N последних)
  --allow-edit разрешить игрокам править структуру мира (разработка, пробная игра)
  --spawn      точка появления игроков, метры: x,y,z
";

fn parse_args() -> Result<ServerConfig, String> {
    let mut args = std::env::args().skip(1);
    let mut cfg = ServerConfig::new("");
    let mut have_dir = false;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("для {name} нужно значение"));
        match arg.as_str() {
            "--data-dir" => {
                cfg.data_dir = PathBuf::from(value("--data-dir")?);
                have_dir = true;
            }
            "--world-id" => cfg.world_id = value("--world-id")?,
            "--tick-hz" => {
                cfg.tick_hz = value("--tick-hz")?
                    .parse()
                    .map_err(|e| format!("--tick-hz: {e}"))?;
                if !(cfg.tick_hz.is_finite() && (1.0..=240.0).contains(&cfg.tick_hz)) {
                    return Err("--tick-hz должен быть в диапазоне 1..=240".into());
                }
            }
            "--ticks" => {
                cfg.max_ticks = Some(
                    value("--ticks")?
                        .parse()
                        .map_err(|e| format!("--ticks: {e}"))?,
                )
            }
            "--script" => cfg.script = Some(PathBuf::from(value("--script")?)),
            "--listen" => cfg.listen = Some(value("--listen")?),
            "--listen-udp" => cfg.listen_udp = Some(value("--listen-udp")?),
            "--key-file" => cfg.key_file = Some(PathBuf::from(value("--key-file")?)),
            "--allow-edit" => cfg.allow_edit = true,
            "--listen-lightyear" => cfg.listen_lightyear = Some(value("--listen-lightyear")?),
            "--ly-key" => {
                cfg.ly_key = Some(
                    astrocore_network::secure::parse_hex32(&value("--ly-key")?)
                        .ok_or("--ly-key: нужно 64 шестнадцатеричных символа")?,
                )
            }
            "--backup-keep" => {
                cfg.backup_keep = value("--backup-keep")?
                    .parse()
                    .map_err(|e| format!("--backup-keep: {e}"))?
            }
            "--spawn" => {
                let v = value("--spawn")?;
                let p: Vec<f32> = v
                    .split(',')
                    .map(|c| c.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("--spawn: {e}"))?;
                let [x, y, z] = p[..] else {
                    return Err("--spawn: ожидается x,y,z".into());
                };
                if ![x, y, z].iter().all(|v| v.is_finite()) {
                    return Err("--spawn: числа должны быть конечными".into());
                }
                cfg.spawn = Some([x, y, z]);
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("неизвестный аргумент {other:?}")),
        }
    }
    if !have_dir {
        return Err("не указан --data-dir".into());
    }
    Ok(cfg)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("ошибка: {msg}\n");
            }
            eprint!("{USAGE}");
            return if msg.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };
    match run(&cfg) {
        Ok(r) => {
            println!(
                "тиков: {}, ревизия: {}, элементов: {}, отклонено команд: {}, хеш: {:#018x}{}",
                r.ticks,
                r.revision,
                r.element_count,
                r.rejected,
                r.content_hash,
                if r.recovered {
                    " (мир восстановлен)"
                } else {
                    " (новый мир)"
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ошибка: {e}");
            ExitCode::FAILURE
        }
    }
}
