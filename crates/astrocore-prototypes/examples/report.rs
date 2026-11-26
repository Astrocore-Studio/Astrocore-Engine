//! Отчёт о загрузке каталога прототипов: сколько записей каждого вида прочитано, какие ошибки
//! найдены и какие компоненты движок ещё не знает.
//!
//! Запуск: `cargo run --release -p astrocore-prototypes --example report -- <каталог>`

use std::path::Path;
use std::time::Instant;

use astrocore_ecs::ComponentRegistry;
use astrocore_prototypes::{LoadReport, Prototypes};

fn main() {
    let dir = std::env::args().nth(1).expect("нужен каталог прототипов");
    let mut prototypes = Prototypes::default();
    let mut report = LoadReport::default();
    let started = Instant::now();
    prototypes.load_dir(Path::new(&dir), &mut report);
    let loaded = started.elapsed();
    prototypes.resolve(&ComponentRegistry::default(), &mut report);
    let total = started.elapsed();

    println!(
        "Файлов: {}, видов: {}, чтение {loaded:.1?}, с наследованием {total:.1?}",
        report.files,
        report.loaded.len()
    );
    let mut kinds: Vec<_> = report.loaded.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    println!("Записей по видам (без абстрактных):");
    for (kind, count) in kinds.iter().take(15) {
        println!("  {count:>6}  {kind}");
    }
    let mut components: Vec<_> = report.unknown_components.iter().collect();
    components.sort_by(|a, b| b.1.cmp(a.1));
    println!(
        "Компонентов без типа Rust: {} (в прототипах сущностей: {} раз)",
        components.len(),
        components.iter().map(|(_, c)| **c).sum::<usize>()
    );
    for (name, count) in components.iter().take(15) {
        println!("  {count:>6}  {name}");
    }
    println!("Ошибок: {}", report.errors.len());
    for error in report.errors.iter().take(40) {
        println!("  {error}");
    }
}
