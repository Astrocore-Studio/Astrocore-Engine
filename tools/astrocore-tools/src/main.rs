use std::path::PathBuf;
use std::process::ExitCode;

use astrocore_tools::{
    bench, compile_project, make_demo_project, upgrade_server_map, validate_project, verify_bundle,
};

const USAGE: &str = "\
astrocore-tools — инструменты Astrocore

  astrocore-tools demo-project <файл.acproj>
  astrocore-tools validate <файл.acproj>
  astrocore-tools compile <файл.acproj> --out <каталог> --game <id>
  astrocore-tools verify <каталог> --game <id>
  astrocore-tools upgrade-map --base <N.acproj> --new <N+1.acproj> --data-dir <каталог> [--prefer-author]
  astrocore-tools bench [--blocks N]
";

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("demo-project") => match args.get(1) {
            Some(p) => make_demo_project(&PathBuf::from(p))
                .map(|()| println!("создан {p}"))
                .map_err(|e| e.to_string()),
            None => Err("не указан файл".into()),
        },
        Some("validate") => match args.get(1) {
            Some(p) => validate_project(&PathBuf::from(p))
                .map(|f| {
                    for x in &f {
                        println!("{:?}: {}", x.severity, x.message);
                    }
                    println!("находок: {} (ошибок нет)", f.len());
                })
                .map_err(|e| e.to_string()),
            None => Err("не указан файл".into()),
        },
        Some("compile") => match (
            args.get(1),
            arg_value(&args, "--out"),
            arg_value(&args, "--game"),
        ) {
            (Some(p), Some(out), Some(game)) => {
                compile_project(&PathBuf::from(p), &PathBuf::from(out), &game)
                    .map(|m| {
                        println!(
                            "собрано: мир {} ревизия {}, хеш мира {:#x}, клиент {}, сервер {}",
                            m.world_id,
                            m.world_revision,
                            m.world_hash,
                            m.client_hash,
                            m.server_hash
                        )
                    })
                    .map_err(|e| e.to_string())
            }
            _ => Err("нужны файл проекта, --out и --game".into()),
        },
        Some("verify") => match (args.get(1), arg_value(&args, "--game")) {
            (Some(dir), Some(game)) => verify_bundle(&PathBuf::from(dir), &game)
                .map(|m| println!("пакеты совместимы: игра {}, мир {}", m.game_id, m.world_id))
                .map_err(|e| e.to_string()),
            _ => Err("нужны каталог и --game".into()),
        },
        Some("upgrade-map") => match (
            arg_value(&args, "--base"),
            arg_value(&args, "--new"),
            arg_value(&args, "--data-dir"),
        ) {
            (Some(base), Some(new), Some(dir)) => {
                let policy = if args.iter().any(|a| a == "--prefer-author") {
                    astrocore_persistence::upgrade::ConflictPolicy::PreferAuthor
                } else {
                    astrocore_persistence::upgrade::ConflictPolicy::KeepRuntime
                };
                upgrade_server_map(
                    &PathBuf::from(base),
                    &PathBuf::from(new),
                    &PathBuf::from(dir),
                    policy,
                )
                .map(|r| {
                    println!("карта обновлена: {}", r.summary());
                    for c in &r.conflicts {
                        println!(
                            "  конфликт {:?} объект {:?} этаж {:?}: {} ({:?})",
                            c.kind, c.id, c.floor, c.detail, c.resolution
                        );
                    }
                })
                .map_err(|e| e.to_string())
            }
            _ => Err("нужны --base, --new и --data-dir".into()),
        },
        Some("bench") => {
            let blocks = arg_value(&args, "--blocks")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8);
            let elements = bench::city_world(blocks).element_count();
            print!(
                "{}",
                bench::format_table(blocks, &bench::run(blocks), elements)
            );
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ошибка: {e}");
            ExitCode::FAILURE
        }
    }
}
