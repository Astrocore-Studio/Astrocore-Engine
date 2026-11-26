//! Проверка загрузки языка из каталога ресурсов.
//!
//! Запуск: `cargo run --release -p astrocore-loc --example scan_locale -- <каталог Resources> [язык]`

use std::time::Instant;

use astrocore_loc::{DEFAULT_CULTURE, Loc};
use astrocore_resources::Resources;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("нужен каталог ресурсов");
    let culture = args.next().unwrap_or_else(|| DEFAULT_CULTURE.to_owned());
    let mut resources = Resources::default();
    resources.mount(&root);
    let mut loc = Loc::default();
    let started = Instant::now();
    let report = loc.load_culture(&resources, &culture).expect("код языка");
    println!(
        "Язык {culture}: файлов {}, сообщений {}, замечаний {} — {:.0?}",
        report.files,
        report.messages,
        report.errors.len(),
        started.elapsed()
    );
    for error in report.errors.iter().take(20) {
        println!("  {error}");
    }
}
