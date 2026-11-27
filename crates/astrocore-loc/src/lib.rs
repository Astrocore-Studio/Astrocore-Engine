//! Локализация: сообщения Fluent (`.ftl`) из `Locale/<язык>/` каталогов ресурсов.
//! Образец — `Robust.Shared/Localization/LocalizationManager.cs` @ 1afd692. Расширения
//! синтаксиса Linguini, которыми пользуются файлы SS14, готовит `compat` (ADR-0018).
//!
//! Сообщение ищется в основном языке, затем в запасном. Ненайденное сообщение возвращает свой
//! идентификатор — так пропуски видны в игре и ничего не падает.

mod compat;
mod names;

pub use names::{EntityNameWorldExt, EntityNames, EntityText};

use std::collections::HashMap;
use std::sync::Arc;

use astrocore_resources::Resources;
use bevy_ecs::resource::Resource;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentError, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// Язык по умолчанию и запасной язык: на нём написаны сообщения SS14.
pub const DEFAULT_CULTURE: &str = "en-US";

/// Аргумент сообщения.
#[derive(Debug, Clone, PartialEq)]
pub enum LocArg {
    Text(String),
    Number(f64),
}

impl From<&str> for LocArg {
    fn from(value: &str) -> Self {
        LocArg::Text(value.to_owned())
    }
}

impl From<String> for LocArg {
    fn from(value: String) -> Self {
        LocArg::Text(value)
    }
}

macro_rules! number_arg {
    ($($ty:ty),*) => {$(
        impl From<$ty> for LocArg {
            fn from(value: $ty) -> Self {
                LocArg::Number(value as f64)
            }
        }
    )*};
}

number_arg!(i32, i64, u32, u64, usize, f32, f64);

/// Функция сообщений: позиционные аргументы → значение.
pub type LocFunction = fn(&[LocArg]) -> LocArg;

#[derive(Debug, thiserror::Error)]
pub enum LocError {
    #[error("неверный код языка {0:?}")]
    BadCulture(String),
}

/// Итог загрузки языка.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LocReport {
    pub files: usize,
    /// Число сообщений языка.
    pub messages: usize,
    /// Ошибки разбора и повторные идентификаторы: `файл: описание`.
    pub errors: Vec<String>,
}

struct Culture {
    bundle: FluentBundle<FluentResource>,
    messages: usize,
}

/// Сообщения игры. Ресурс `World`.
#[derive(Resource)]
pub struct Loc {
    /// Основной язык, затем запасные.
    cultures: Vec<(String, Culture)>,
    functions: HashMap<&'static str, LocFunction>,
}

impl Default for Loc {
    fn default() -> Self {
        let mut loc = Self {
            cultures: Vec::new(),
            functions: HashMap::new(),
        };
        loc.add_function("CAPITALIZE", capitalize);
        loc.add_function("TOSTRING", to_string);
        loc
    }
}

fn to_fluent(arg: &LocArg) -> FluentValue<'static> {
    match arg {
        LocArg::Text(text) => FluentValue::from(text.clone()),
        LocArg::Number(number) => FluentValue::from(*number),
    }
}

fn from_fluent(value: &FluentValue<'_>) -> LocArg {
    match value {
        FluentValue::Number(number) => LocArg::Number(number.value),
        FluentValue::String(text) => LocArg::Text(text.to_string()),
        _ => LocArg::Text(String::new()),
    }
}

/// `CAPITALIZE(текст)`: первая буква — заглавная.
fn capitalize(args: &[LocArg]) -> LocArg {
    let text = match args.first() {
        Some(LocArg::Text(text)) => text.clone(),
        Some(LocArg::Number(number)) => number.to_string(),
        None => String::new(),
    };
    let mut chars = text.chars();
    LocArg::Text(match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    })
}

/// `TOSTRING(значение)`: значение как текст.
fn to_string(args: &[LocArg]) -> LocArg {
    LocArg::Text(match args.first() {
        Some(LocArg::Text(text)) => text.clone(),
        Some(LocArg::Number(number)) => number.to_string(),
        None => String::new(),
    })
}

fn install(bundle: &mut FluentBundle<FluentResource>, name: &'static str, function: LocFunction) {
    let _ = bundle.add_function(name, move |positional, _named| {
        let args: Vec<LocArg> = positional.iter().map(from_fluent).collect();
        to_fluent(&function(&args))
    });
}

impl Loc {
    /// Добавляет функцию сообщений, например `THE` или `GENDER`. Действует для языков,
    /// загруженных после вызова, и для уже загруженных.
    pub fn add_function(&mut self, name: &'static str, function: LocFunction) {
        self.functions.insert(name, function);
        for (_, culture) in &mut self.cultures {
            install(&mut culture.bundle, name, function);
        }
    }

    /// Загружает сообщения языка из `Locale/<язык>/` всех каталогов ресурсов. Первый
    /// загруженный язык — основной, следующие — запасные.
    pub fn load_culture(
        &mut self,
        resources: &Resources,
        code: &str,
    ) -> Result<LocReport, LocError> {
        let language: LanguageIdentifier = code
            .parse()
            .map_err(|_| LocError::BadCulture(code.to_owned()))?;
        let mut bundle = FluentBundle::new_concurrent(vec![language]);
        // Знаки изоляции направления письма мешают сравнению и поиску строк.
        bundle.set_use_isolating(false);
        for (name, function) in &self.functions {
            install(&mut bundle, name, *function);
        }
        let mut report = LocReport::default();
        let files = resources
            .find_files(&format!("Locale/{code}"))
            .unwrap_or_default();
        let mut texts = Vec::new();
        for path in files.iter().filter(|path| path.ends_with(".ftl")) {
            report.files += 1;
            match resources.read_to_string(path) {
                Ok(text) => texts.push((path, compat::strip_bom(&text).to_owned())),
                Err(error) => report.errors.push(format!("{path}: {error}")),
            }
        }
        // Термины всех файлов языка: вызов может стоять в другом файле, чем определение.
        let mut terms = HashMap::new();
        for (_, text) in &texts {
            compat::collect_terms(text, &mut terms);
        }
        let mut generated_count = 0;
        for (path, text) in texts {
            let (text, generated) = compat::rewrite_term_calls(&text, &terms, &mut generated_count);
            let resource = match FluentResource::try_new(text) {
                Ok(resource) => resource,
                Err((resource, errors)) => {
                    report
                        .errors
                        .extend(errors.iter().map(|error| format!("{path}: {error}")));
                    resource
                }
            };
            // Служебные сообщения из вызовов терминов не считаются.
            report.messages += resource
                .entries()
                .filter(|entry| matches!(entry, fluent_syntax::ast::Entry::Message(_)))
                .count()
                .saturating_sub(generated);
            if let Err(errors) = bundle.add_resource(resource) {
                for error in errors {
                    if let FluentError::Overriding { id, .. } = error {
                        report.messages = report.messages.saturating_sub(1);
                        report
                            .errors
                            .push(format!("{path}: сообщение {id} объявлено дважды"));
                    }
                }
            }
        }
        let culture = Culture {
            bundle,
            messages: report.messages,
        };
        match self
            .cultures
            .iter_mut()
            .find(|(existing, _)| existing == code)
        {
            Some((_, slot)) => *slot = culture,
            None => self.cultures.push((code.to_owned(), culture)),
        }
        Ok(report)
    }

    /// Загруженные языки: основной первым.
    pub fn cultures(&self) -> impl Iterator<Item = (&str, usize)> {
        self.cultures
            .iter()
            .map(|(code, culture)| (code.as_str(), culture.messages))
    }

    /// Есть ли сообщение хотя бы в одном языке.
    pub fn has(&self, id: &str) -> bool {
        let (message, _) = split_id(id);
        self.cultures
            .iter()
            .any(|(_, culture)| culture.bundle.has_message(message))
    }

    /// Текст сообщения без аргументов; ненайденное сообщение возвращает идентификатор.
    pub fn get(&self, id: &str) -> String {
        self.format(id, &[])
    }

    /// Текст сообщения, если оно есть.
    pub fn try_get(&self, id: &str, args: &[(&str, LocArg)]) -> Option<String> {
        let (message, attribute) = split_id(id);
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, to_fluent(value));
        }
        for (_, culture) in &self.cultures {
            let Some(found) = culture.bundle.get_message(message) else {
                continue;
            };
            let pattern = match attribute {
                Some(attribute) => found.get_attribute(attribute).map(|a| a.value()),
                None => found.value(),
            };
            let Some(pattern) = pattern else {
                continue;
            };
            let mut errors = Vec::new();
            let text = culture
                .bundle
                .format_pattern(pattern, Some(&fluent_args), &mut errors);
            return Some(text.into_owned());
        }
        None
    }

    /// Текст сообщения с аргументами: `loc.format("greeting", &[("name", "Urist".into())])`.
    /// Идентификатор вида `сообщение.атрибут` выбирает атрибут сообщения.
    pub fn format(&self, id: &str, args: &[(&str, LocArg)]) -> String {
        self.try_get(id, args).unwrap_or_else(|| id.to_owned())
    }

    /// Название прототипа сущности: сообщение `ent-<идентификатор>`.
    pub fn entity_name(&self, prototype: &str) -> Option<String> {
        self.try_get(&format!("ent-{prototype}"), &[])
    }

    /// Описание прототипа сущности: атрибут `.desc` сообщения `ent-<идентификатор>`.
    pub fn entity_description(&self, prototype: &str) -> Option<String> {
        self.try_get(&format!("ent-{prototype}.desc"), &[])
    }
}

fn split_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once('.') {
        Some((message, attribute)) => (message, Some(attribute)),
        None => (id, None),
    }
}

/// Общий указатель на сообщения для потоков вне мира (например, для интерфейса).
pub type SharedLoc = Arc<Loc>;

#[cfg(test)]
mod tests {
    use super::*;

    fn resources() -> (tempfile::TempDir, Resources) {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write(
            "Locale/en-US/items.ftl",
            "ent-Crowbar = crowbar\n    .desc = A multipurpose tool to pry open doors.\n    .suffix = Red\nhello = Hello, { $name }!\ncount = { $n ->\n    [one] one item\n   *[other] { $n } items\n}\nshout = { CAPITALIZE($word) } and { LOUD($word) }\nonly-english = fallback\n",
        );
        write(
            "Locale/ru-RU/sub/items.ftl",
            "ent-Crowbar = лом\n    .desc = Универсальный инструмент.\nhello = Привет, { $name }!\ncount = { $n ->\n    [one] { $n } предмет\n    [few] { $n } предмета\n   *[other] { $n } предметов\n}\nhello = повтор\nbroken = { $\n",
        );
        write("Locale/ru-RU/readme.txt", "не сообщения");
        let mut resources = Resources::default();
        resources.mount(dir.path());
        (dir, resources)
    }

    #[test]
    fn term_calls_with_variables_and_bom_load_like_linguini() {
        let dir = tempfile::tempdir().unwrap();
        let locale = dir.path().join("Locale/en-US");
        std::fs::create_dir_all(&locale).unwrap();
        std::fs::write(
            locale.join("terms.ftl"),
            "-selection = { $selectable ->\n    [one] one more\n   *[other] { $selectable } more\n}\n-verb = { $satiated ->\n    [true] {\" \"}No { $verb }.\n  *[false] {\"\"}\n}\n",
        )
        .unwrap();
        std::fs::write(
            locale.join("uses.ftl"),
            "\u{FEFF}limits = Up to { $count }. { -selection(selectable: $left) }\nnom = Nom.{ -verb(satiated: $satiated, verb: \"eat\") }\n",
        )
        .unwrap();
        let mut resources = Resources::default();
        resources.mount(dir.path());
        let mut loc = Loc::default();
        let report = loc.load_culture(&resources, "en-US").unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.messages, 2);
        assert_eq!(
            loc.format(
                "limits",
                &[("count", LocArg::from(3)), ("left", LocArg::from(2))]
            ),
            "Up to 3. 2 more"
        );
        assert_eq!(
            loc.format("nom", &[("satiated", LocArg::from("true"))]),
            "Nom. No eat."
        );
        assert_eq!(
            loc.format("nom", &[("satiated", LocArg::from("false"))]),
            "Nom."
        );
    }

    #[test]
    fn messages_plurals_attributes_and_fallback() {
        let (_dir, resources) = resources();
        let mut loc = Loc::default();
        let russian = loc.load_culture(&resources, "ru-RU").unwrap();
        assert_eq!(russian.files, 1);
        assert_eq!(russian.messages, 3);
        assert!(
            russian
                .errors
                .iter()
                .any(|e| e.contains("hello объявлено дважды"))
        );
        assert!(russian.errors.len() >= 2, "{:?}", russian.errors);
        let english = loc.load_culture(&resources, DEFAULT_CULTURE).unwrap();
        assert_eq!((english.messages, english.errors.len()), (5, 0));

        assert_eq!(
            loc.format("hello", &[("name", "Урист".into())]),
            "Привет, Урист!"
        );
        assert_eq!(loc.format("count", &[("n", 1.into())]), "1 предмет");
        assert_eq!(loc.format("count", &[("n", 3.into())]), "3 предмета");
        assert_eq!(loc.format("count", &[("n", 25.into())]), "25 предметов");
        assert_eq!(loc.entity_name("Crowbar").as_deref(), Some("лом"));
        assert_eq!(
            loc.entity_description("Crowbar").as_deref(),
            Some("Универсальный инструмент.")
        );
        // Атрибута и сообщения нет в русском — берётся английский.
        assert_eq!(loc.get("ent-Crowbar.suffix"), "Red");
        assert_eq!(loc.get("only-english"), "fallback");
        assert_eq!(loc.get("missing-id"), "missing-id");
        assert!(loc.has("hello") && !loc.has("missing-id"));
        assert!(loc.entity_name("Wrench").is_none());
        let cultures: Vec<_> = loc.cultures().collect();
        assert_eq!(cultures, [("ru-RU", 3), ("en-US", 5)]);
        assert!(matches!(
            loc.load_culture(&resources, "не язык"),
            Err(LocError::BadCulture(_))
        ));
    }

    #[test]
    fn functions_builtin_and_custom() {
        let (_dir, resources) = resources();
        let mut loc = Loc::default();
        loc.load_culture(&resources, DEFAULT_CULTURE).unwrap();
        // Неизвестная функция не роняет форматирование.
        let before = loc.format("shout", &[("word", "hey".into())]);
        assert!(before.starts_with("Hey and "), "{before}");
        loc.add_function("LOUD", |args| match args.first() {
            Some(LocArg::Text(text)) => LocArg::Text(text.to_uppercase()),
            _ => LocArg::Text(String::new()),
        });
        assert_eq!(
            loc.format("shout", &[("word", "hey".into())]),
            "Hey and HEY"
        );
        assert_eq!(loc.format("count", &[("n", 1.into())]), "one item");
        assert_eq!(loc.format("count", &[("n", 2.5.into())]), "2.5 items");
    }
}
