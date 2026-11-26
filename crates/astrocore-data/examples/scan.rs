//! Проверка разбора на настоящих данных: читает все файлы YAML и RON каталога, сообщает ошибки
//! и проверяет, что запись в RON и YAML читается обратно в то же дерево.
//!
//! Запуск: `cargo run --release -p astrocore-data --example scan -- <каталог>`

use std::path::{Path, PathBuf};
use std::time::Instant;

use astrocore_data::{
    DataFormat, DataNode, Value, parse_ron, parse_yaml, to_ron_string, to_yaml_string,
};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if DataFormat::from_path(&path).is_some() {
            out.push(path);
        }
    }
}

/// Дерево без признака кавычек и с одним видом пустого значения. Пустое значение с тегом
/// (`!type:Foo`) равнозначно пустому отображению с тегом (`Foo()` в RON).
fn normalize(node: &mut DataNode) {
    let null = node.is_null();
    if null && node.tag.is_some() {
        node.value = Value::Map(Default::default());
        return;
    }
    match &mut node.value {
        Value::Scalar(scalar) => {
            if null {
                scalar.text = "null".into();
            } else if !scalar.quoted && matches!(scalar.text.as_str(), "True" | "False") {
                scalar.text = scalar.text.to_lowercase();
            }
            // Пустая строка в кавычках — строка, без кавычек — пустое значение.
            scalar.quoted = scalar.text.is_empty();
        }
        Value::Seq(items) => items.iter_mut().for_each(normalize),
        Value::Map(map) => map.0.iter_mut().for_each(|(_, n)| normalize(n)),
    }
}

/// Путь к первому расхождению двух деревьев.
fn first_difference(a: &DataNode, b: &DataNode, path: &str) -> Option<String> {
    if a.tag != b.tag {
        return Some(format!("{path}: тег {:?} и {:?}", a.tag, b.tag));
    }
    match (&a.value, &b.value) {
        (Value::Scalar(x), Value::Scalar(y)) if x == y => None,
        (Value::Seq(x), Value::Seq(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (x, y))| first_difference(x, y, &format!("{path}[{i}]"))),
        (Value::Map(x), Value::Map(y)) if x.len() == y.len() => {
            x.0.iter().zip(&y.0).find_map(|((kx, x), (ky, y))| {
                if kx != ky {
                    return Some(format!("{path}: ключ {kx:?} и {ky:?}"));
                }
                first_difference(x, y, &format!("{path}.{kx}"))
            })
        }
        _ => Some(
            format!("{path}: {:?} и {:?}", a.value, b.value)
                .chars()
                .take(300)
                .collect(),
        ),
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("нужен каталог");
    let mut files = Vec::new();
    collect(Path::new(&root), &mut files);
    files.sort();
    let started = Instant::now();
    let (mut bytes, mut failed, mut mismatched) = (0usize, 0usize, 0usize);
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            println!("{}: не читается как UTF-8", path.display());
            failed += 1;
            continue;
        };
        bytes += text.len();
        let format = DataFormat::from_path(path).unwrap();
        let mut tree = match format.parse(&text) {
            Ok(tree) => tree,
            Err(error) => {
                println!("{}: {error}", path.display());
                failed += 1;
                continue;
            }
        };
        normalize(&mut tree);
        let ron = parse_ron(&to_ron_string(&tree)).map(|mut t| {
            normalize(&mut t);
            t
        });
        let yaml = parse_yaml(&to_yaml_string(&tree)).map(|mut t| {
            normalize(&mut t);
            t
        });
        if ron.as_ref() != Ok(&tree) || yaml.as_ref() != Ok(&tree) {
            println!(
                "{}: запись не читается обратно (RON: {}, YAML: {})",
                path.display(),
                ron.map_or_else(
                    |e| e.to_string(),
                    |t| first_difference(&tree, &t, "").unwrap_or_else(|| "совпадает".into())
                ),
                yaml.map_or_else(
                    |e| e.to_string(),
                    |t| first_difference(&tree, &t, "").unwrap_or_else(|| "совпадает".into())
                ),
            );
            mismatched += 1;
        }
    }
    println!(
        "Файлов: {}, байт: {bytes}, ошибок разбора: {failed}, расхождений записи: {mismatched}, время: {:.1?}",
        files.len(),
        started.elapsed()
    );
}
