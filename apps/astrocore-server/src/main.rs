use std::path::PathBuf;
use std::process::ExitCode;

use astrocore_server::{ServerConfig, run};

const USAGE: &str = "\
astrocore-server — headless-сервер Astrocore

Использование:
  astrocore-server --data-dir <каталог> [--world-id <id>] [--tick-hz <гц>]
                   [--ticks <N>] [--script <команды.json>]

  --data-dir   каталог сохранений (снимок + журнал); создаётся при отсутствии
  --world-id   идентификатор нового мира (по умолчанию world)
  --tick-hz    частота фиксированного шага, Гц (по умолчанию 20)
  --ticks      выполнить N тиков и выйти (по умолчанию — работать до остановки)
  --script     JSON-массив WorldCommand для постановки в очередь при старте
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
