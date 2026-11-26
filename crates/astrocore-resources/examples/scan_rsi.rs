//! Проверка разбора `meta.json` всех наборов RSI каталога ресурсов.
//!
//! Запуск: `cargo run --release -p astrocore-resources --example scan_rsi -- <каталог Resources>`

use astrocore_resources::Resources;

fn main() {
    let root = std::env::args().nth(1).expect("нужен каталог ресурсов");
    let mut resources = Resources::default();
    resources.mount(&root);
    let files = resources
        .find_files("Textures")
        .expect("каталог Textures читается");
    let (mut sets, mut states, mut failed) = (0usize, 0usize, 0usize);
    for meta in files.iter().filter(|path| path.ends_with(".rsi/meta.json")) {
        let rsi = meta.trim_end_matches("/meta.json");
        match resources.rsi_meta(rsi) {
            Ok(parsed) => {
                sets += 1;
                states += parsed.states.len();
                for state in &parsed.states {
                    if !resources.exists(&format!("{rsi}/{}.png", state.name)) {
                        println!("{rsi}: нет картинки состояния {}", state.name);
                        failed += 1;
                    }
                }
            }
            Err(error) => {
                println!("{rsi}: {error}");
                failed += 1;
            }
        }
    }
    println!("Наборов RSI: {sets}, состояний: {states}, замечаний: {failed}");
}
