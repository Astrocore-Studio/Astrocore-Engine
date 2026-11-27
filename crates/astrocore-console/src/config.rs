//! Настройки (CVar). Образец — `Robust.Shared/Configuration` @ 1afd692.
//!
//! Настройка объявляется константой [`CVarDef`] и регистрируется в [`Config`]. Значения
//! приходят из файла TOML (`[net]` + `tickrate = 30` → `net.tickrate`), из командной строки
//! и из консоли. Значение, заданное раньше регистрации, ждёт её и применяется при ней.

use std::collections::BTreeMap;
use std::fmt;

use astrocore_ecs::EventWorldExt;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;

/// Флаги настройки (`CVar` Robust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CVarFlags(pub u16);

impl CVarFlags {
    pub const NONE: CVarFlags = CVarFlags(0);
    /// Меняется только с разрешёнными читами.
    pub const CHEAT: CVarFlags = CVarFlags(1);
    /// Значение задаёт сервер.
    pub const SERVER: CVarFlags = CVarFlags(2);
    /// Меняется только вне соединения с сервером.
    pub const NOT_CONNECTED: CVarFlags = CVarFlags(4);
    /// Сервер передаёт значение клиентам.
    pub const REPLICATED: CVarFlags = CVarFlags(8);
    /// Сохраняется в файл настроек.
    pub const ARCHIVE: CVarFlags = CVarFlags(16);
    /// Об изменении сообщается игрокам.
    pub const NOTIFY: CVarFlags = CVarFlags(32);
    /// Существует только на сервере.
    pub const SERVERONLY: CVarFlags = CVarFlags(64);
    /// Существует только на клиенте.
    pub const CLIENTONLY: CVarFlags = CVarFlags(128);
    /// Значение не показывается в журналах и списках (пароли, ключи).
    pub const CONFIDENTIAL: CVarFlags = CVarFlags(256);
    /// Клиент передаёт значение серверу.
    pub const CLIENT: CVarFlags = CVarFlags(512);

    pub const fn union(self, other: CVarFlags) -> CVarFlags {
        CVarFlags(self.0 | other.0)
    }

    pub const fn contains(self, other: CVarFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Значение настройки.
#[derive(Debug, Clone, PartialEq)]
pub enum CVarValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl fmt::Display for CVarValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CVarValue::Bool(v) => write!(f, "{v}"),
            CVarValue::Int(v) => write!(f, "{v}"),
            CVarValue::Float(v) => write!(f, "{v}"),
            CVarValue::Text(v) => f.write_str(v),
        }
    }
}

impl CVarValue {
    fn kind(&self) -> &'static str {
        match self {
            CVarValue::Bool(_) => "bool",
            CVarValue::Int(_) => "int",
            CVarValue::Float(_) => "float",
            CVarValue::Text(_) => "string",
        }
    }

    /// Приводит значение к виду образца: целое к дробному, текст — разбором.
    pub fn coerce(self, like: &CVarValue) -> Result<CVarValue, ConfigError> {
        let mismatch = |value: &CVarValue| ConfigError::WrongType {
            expected: like.kind(),
            found: value.to_string(),
        };
        Ok(match (like, self) {
            (CVarValue::Bool(_), CVarValue::Bool(v)) => CVarValue::Bool(v),
            (CVarValue::Int(_), CVarValue::Int(v)) => CVarValue::Int(v),
            (CVarValue::Float(_), CVarValue::Float(v)) => CVarValue::Float(v),
            (CVarValue::Float(_), CVarValue::Int(v)) => CVarValue::Float(v as f64),
            (CVarValue::Text(_), CVarValue::Text(v)) => CVarValue::Text(v),
            (CVarValue::Bool(_), CVarValue::Text(text)) => match text.trim() {
                "true" | "True" | "1" => CVarValue::Bool(true),
                "false" | "False" | "0" => CVarValue::Bool(false),
                _ => return Err(mismatch(&CVarValue::Text(text))),
            },
            (CVarValue::Int(_), CVarValue::Text(text)) => match text.trim().parse() {
                Ok(v) => CVarValue::Int(v),
                Err(_) => return Err(mismatch(&CVarValue::Text(text))),
            },
            (CVarValue::Float(_), CVarValue::Text(text)) => match text.trim().parse() {
                Ok(v) => CVarValue::Float(v),
                Err(_) => return Err(mismatch(&CVarValue::Text(text))),
            },
            (_, other) => return Err(mismatch(&other)),
        })
    }
}

/// Тип Rust, которым читается настройка.
pub trait CVarType: Sized + Clone {
    fn into_value(self) -> CVarValue;
    fn from_value(value: &CVarValue) -> Option<Self>;
}

impl CVarType for bool {
    fn into_value(self) -> CVarValue {
        CVarValue::Bool(self)
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Bool(v) => Some(*v),
            _ => None,
        }
    }
}

impl CVarType for i64 {
    fn into_value(self) -> CVarValue {
        CVarValue::Int(self)
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Int(v) => Some(*v),
            _ => None,
        }
    }
}

impl CVarType for i32 {
    fn into_value(self) -> CVarValue {
        CVarValue::Int(i64::from(self))
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Int(v) => i32::try_from(*v).ok(),
            _ => None,
        }
    }
}

impl CVarType for f32 {
    fn into_value(self) -> CVarValue {
        CVarValue::Float(f64::from(self))
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Float(v) => Some(*v as f32),
            _ => None,
        }
    }
}

impl CVarType for f64 {
    fn into_value(self) -> CVarValue {
        CVarValue::Float(self)
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Float(v) => Some(*v),
            _ => None,
        }
    }
}

impl CVarType for String {
    fn into_value(self) -> CVarValue {
        CVarValue::Text(self)
    }

    fn from_value(value: &CVarValue) -> Option<Self> {
        match value {
            CVarValue::Text(v) => Some(v.clone()),
            _ => None,
        }
    }
}

impl CVarType for &'static str {
    fn into_value(self) -> CVarValue {
        CVarValue::Text(self.to_owned())
    }

    fn from_value(_: &CVarValue) -> Option<Self> {
        None
    }
}

/// Объявление настройки: `pub const NET_TICKRATE: CVarDef<i32> = CVarDef::new(...)`.
#[derive(Debug, Clone, Copy)]
pub struct CVarDef<T> {
    pub name: &'static str,
    pub default: T,
    pub flags: CVarFlags,
    pub description: &'static str,
}

impl<T> CVarDef<T> {
    pub const fn new(
        name: &'static str,
        default: T,
        flags: CVarFlags,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            default,
            flags,
            description,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("настройка {0} не зарегистрирована")]
    Unknown(String),
    #[error("ожидалось значение вида {expected}, получено `{found}`")]
    WrongType {
        expected: &'static str,
        found: String,
    },
    #[error("файл настроек: {0}")]
    Parse(String),
}

#[derive(Debug, Clone)]
struct Entry {
    value: CVarValue,
    default: CVarValue,
    flags: CVarFlags,
    description: &'static str,
}

/// Настройка изменилась. Широковещательно.
#[derive(Debug, Clone)]
pub struct CVarChanged {
    pub name: String,
    pub value: CVarValue,
}

/// Сведения о настройке для списков и справки.
#[derive(Debug, Clone, PartialEq)]
pub struct CVarInfo<'a> {
    pub name: &'a str,
    pub value: &'a CVarValue,
    pub default: &'a CVarValue,
    pub flags: CVarFlags,
    pub description: &'a str,
}

/// Все настройки. Ресурс `World`.
#[derive(Resource, Debug, Default)]
pub struct Config {
    entries: BTreeMap<String, Entry>,
    /// Значения, заданные до регистрации настройки.
    pending: BTreeMap<String, CVarValue>,
}

impl Config {
    /// Регистрирует настройку. Значение, заданное раньше (из файла или командной строки),
    /// применяется сразу.
    pub fn register<T: CVarType>(&mut self, def: &CVarDef<T>) -> Result<(), ConfigError> {
        if self.entries.contains_key(def.name) {
            return Ok(());
        }
        let default = def.default.clone().into_value();
        let value = match self.pending.remove(def.name) {
            Some(pending) => pending.coerce(&default)?,
            None => default.clone(),
        };
        self.entries.insert(
            def.name.to_owned(),
            Entry {
                value,
                default,
                flags: def.flags,
                description: def.description,
            },
        );
        Ok(())
    }

    /// Значение настройки; незарегистрированная настройка даёт значение по умолчанию.
    pub fn get<T: CVarType + CVarOwned>(&self, def: &CVarDef<T>) -> T {
        self.entries
            .get(def.name)
            .and_then(|entry| T::from_value(&entry.value))
            .unwrap_or_else(|| def.default.clone())
    }

    /// Текстовая настройка, объявленная со значением `&'static str`.
    pub fn get_text(&self, def: &CVarDef<&'static str>) -> String {
        self.entries
            .get(def.name)
            .and_then(|entry| String::from_value(&entry.value))
            .unwrap_or_else(|| def.default.to_owned())
    }

    /// Задаёт значение по имени. Для незарегистрированной настройки значение запоминается
    /// до её регистрации. Возвращает новое значение, если оно изменилось.
    pub fn set_value(
        &mut self,
        name: &str,
        value: CVarValue,
    ) -> Result<Option<CVarValue>, ConfigError> {
        let Some(entry) = self.entries.get_mut(name) else {
            self.pending.insert(name.to_owned(), value);
            return Ok(None);
        };
        let value = value.coerce(&entry.default)?;
        if entry.value == value {
            return Ok(None);
        }
        entry.value = value.clone();
        Ok(Some(value))
    }

    /// Сведения о зарегистрированной настройке.
    pub fn info(&self, name: &str) -> Option<CVarInfo<'_>> {
        self.entries
            .get_key_value(name)
            .map(|(name, entry)| CVarInfo {
                name,
                value: &entry.value,
                default: &entry.default,
                flags: entry.flags,
                description: entry.description,
            })
    }

    /// Зарегистрированные настройки по алфавиту.
    pub fn iter(&self) -> impl Iterator<Item = CVarInfo<'_>> {
        self.entries.iter().map(|(name, entry)| CVarInfo {
            name,
            value: &entry.value,
            default: &entry.default,
            flags: entry.flags,
            description: entry.description,
        })
    }

    /// Имена настроек, заданных в файле или командной строке, но не зарегистрированных.
    pub fn unregistered(&self) -> impl Iterator<Item = &str> {
        self.pending.keys().map(String::as_str)
    }

    /// Читает значения из текста TOML. Возвращает изменившиеся настройки.
    pub fn load_toml(&mut self, text: &str) -> Result<Vec<CVarChanged>, ConfigError> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;
        let mut flat = Vec::new();
        flatten(&table, String::new(), &mut flat);
        let mut changed = Vec::new();
        for (name, value) in flat {
            if let Some(value) = self.set_value(&name, value)? {
                changed.push(CVarChanged { name, value });
            }
        }
        Ok(changed)
    }

    /// Записывает в TOML настройки с флагом `ARCHIVE`, отличающиеся от значений по умолчанию.
    pub fn save_toml(&self) -> String {
        let mut root = toml::Table::new();
        for (name, entry) in &self.entries {
            if !entry.flags.contains(CVarFlags::ARCHIVE) || entry.value == entry.default {
                continue;
            }
            let (section, key) = name.rsplit_once('.').unwrap_or(("", name));
            let mut table = &mut root;
            for part in section.split('.').filter(|part| !part.is_empty()) {
                table = table
                    .entry(part)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .expect("раздел настроек — таблица");
            }
            let value = match &entry.value {
                CVarValue::Bool(v) => toml::Value::Boolean(*v),
                CVarValue::Int(v) => toml::Value::Integer(*v),
                CVarValue::Float(v) => toml::Value::Float(*v),
                CVarValue::Text(v) => toml::Value::String(v.clone()),
            };
            table.insert(key.to_owned(), value);
        }
        root.to_string()
    }
}

/// Типы, которыми настройка читается владением (все, кроме `&'static str`).
pub trait CVarOwned {}
impl CVarOwned for bool {}
impl CVarOwned for i32 {}
impl CVarOwned for i64 {}
impl CVarOwned for f32 {}
impl CVarOwned for f64 {}
impl CVarOwned for String {}

fn flatten(table: &toml::Table, prefix: String, out: &mut Vec<(String, CVarValue)>) {
    for (key, value) in table {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) => flatten(inner, name, out),
            toml::Value::Boolean(v) => out.push((name, CVarValue::Bool(*v))),
            toml::Value::Integer(v) => out.push((name, CVarValue::Int(*v))),
            toml::Value::Float(v) => out.push((name, CVarValue::Float(*v))),
            toml::Value::String(v) => out.push((name, CVarValue::Text(v.clone()))),
            other => out.push((name, CVarValue::Text(other.to_string()))),
        }
    }
}

/// Настройки на `World`.
pub trait ConfigWorldExt {
    /// Регистрирует настройку.
    fn register_cvar<T: CVarType>(&mut self, def: &CVarDef<T>);

    /// Значение настройки.
    fn cvar<T: CVarType + CVarOwned>(&self, def: &CVarDef<T>) -> T;

    /// Задаёт значение и поднимает [`CVarChanged`], если оно изменилось.
    fn set_cvar<T: CVarType>(&mut self, def: &CVarDef<T>, value: T) -> Result<(), ConfigError>;

    /// Задаёт значение по имени из текста (консоль, командная строка).
    fn set_cvar_text(&mut self, name: &str, value: &str) -> Result<(), ConfigError>;

    /// Читает файл настроек TOML и поднимает [`CVarChanged`] для изменившихся.
    fn load_config(&mut self, toml: &str) -> Result<(), ConfigError>;
}

impl ConfigWorldExt for World {
    fn register_cvar<T: CVarType>(&mut self, def: &CVarDef<T>) {
        let mut config = self.get_resource_or_insert_with(Config::default);
        if let Err(error) = config.register(def) {
            // Негодное значение из файла не должно ронять запуск: остаётся значение по умолчанию.
            config.pending.remove(def.name);
            let _ = config.register(def);
            eprintln!(
                "настройка {}: {error}; взято значение по умолчанию",
                def.name
            );
        }
    }

    fn cvar<T: CVarType + CVarOwned>(&self, def: &CVarDef<T>) -> T {
        match self.get_resource::<Config>() {
            Some(config) => config.get(def),
            None => def.default.clone(),
        }
    }

    fn set_cvar<T: CVarType>(&mut self, def: &CVarDef<T>, value: T) -> Result<(), ConfigError> {
        let mut config = self.get_resource_or_insert_with(Config::default);
        if let Some(value) = config.set_value(def.name, value.into_value())? {
            self.raise_broadcast(&mut CVarChanged {
                name: def.name.to_owned(),
                value,
            });
        }
        Ok(())
    }

    fn set_cvar_text(&mut self, name: &str, value: &str) -> Result<(), ConfigError> {
        let mut config = self.get_resource_or_insert_with(Config::default);
        if !config.entries.contains_key(name) {
            return Err(ConfigError::Unknown(name.to_owned()));
        }
        if let Some(value) = config.set_value(name, CVarValue::Text(value.to_owned()))? {
            self.raise_broadcast(&mut CVarChanged {
                name: name.to_owned(),
                value,
            });
        }
        Ok(())
    }

    fn load_config(&mut self, toml: &str) -> Result<(), ConfigError> {
        let mut config = self.get_resource_or_insert_with(Config::default);
        for mut change in config.load_toml(toml)? {
            self.raise_broadcast(&mut change);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKRATE: CVarDef<i32> = CVarDef::new(
        "net.tickrate",
        30,
        CVarFlags::SERVER.union(CVarFlags::REPLICATED),
        "Тиков симуляции в секунду.",
    );
    const HOSTNAME: CVarDef<&str> = CVarDef::new(
        "game.hostname",
        "Astrocore",
        CVarFlags::ARCHIVE,
        "Название сервера.",
    );
    const VOLUME: CVarDef<f32> =
        CVarDef::new("audio.master_volume", 0.5, CVarFlags::ARCHIVE, "Громкость.");
    const LOBBY: CVarDef<bool> = CVarDef::new("game.lobbyenabled", true, CVarFlags::NONE, "Лобби.");

    #[test]
    fn values_come_from_defaults_file_and_setters() {
        let mut world = World::new();
        let mut changes = Vec::new();
        world.register_cvar(&TICKRATE);
        world.register_cvar(&HOSTNAME);
        assert_eq!(world.cvar(&TICKRATE), 30);
        assert!(world.cvar(&LOBBY));

        world
            .load_config(
                "[net]\ntickrate = 60\n[game]\nhostname = \"Relay\"\nlobbyenabled = false\n[audio]\nmaster_volume = 1\n",
            )
            .unwrap();
        assert_eq!(world.cvar(&TICKRATE), 60);
        assert_eq!(world.resource::<Config>().get_text(&HOSTNAME), "Relay");
        // Значения из файла ждут регистрации настройки.
        let config = world.resource::<Config>();
        assert_eq!(
            config.unregistered().collect::<Vec<_>>(),
            ["audio.master_volume", "game.lobbyenabled"]
        );
        world.register_cvar(&LOBBY);
        world.register_cvar(&VOLUME);
        assert!(!world.cvar(&LOBBY));
        assert_eq!(world.cvar(&VOLUME), 1.0);

        world.subscribe_broadcast::<CVarChanged>(|_, _| {});
        world.set_cvar(&TICKRATE, 20).unwrap();
        world.set_cvar_text("net.tickrate", "25").unwrap();
        assert_eq!(world.cvar(&TICKRATE), 25);
        assert_eq!(
            world.set_cvar_text("net.tickrate", "fast"),
            Err(ConfigError::WrongType {
                expected: "int",
                found: "fast".into()
            })
        );
        assert_eq!(
            world.set_cvar_text("net.missing", "1"),
            Err(ConfigError::Unknown("net.missing".into()))
        );
        changes.push(world.cvar(&TICKRATE));
        assert_eq!(changes, [25]);
    }

    #[test]
    fn change_event_fires_only_on_real_change() {
        use bevy_ecs::resource::Resource;

        #[derive(Resource, Default)]
        struct Seen(Vec<String>);

        let mut world = World::new();
        world.init_resource::<Seen>();
        world.register_cvar(&TICKRATE);
        world.subscribe_broadcast::<CVarChanged>(|w, ev| {
            w.resource_mut::<Seen>()
                .0
                .push(format!("{}={}", ev.name, ev.value));
        });
        world.set_cvar(&TICKRATE, 30).unwrap();
        world.set_cvar(&TICKRATE, 45).unwrap();
        world.load_config("[net]\ntickrate = 45\n").unwrap();
        assert_eq!(world.resource::<Seen>().0, ["net.tickrate=45"]);
    }

    #[test]
    fn archive_saves_only_changed_archived_values() {
        let mut config = Config::default();
        config.register(&TICKRATE).unwrap();
        config.register(&HOSTNAME).unwrap();
        config.register(&VOLUME).unwrap();
        config
            .set_value("net.tickrate", CVarValue::Int(10))
            .unwrap();
        config
            .set_value("game.hostname", CVarValue::Text("Relay Two".into()))
            .unwrap();
        let saved = config.save_toml();
        assert_eq!(saved.trim(), "[game]\nhostname = \"Relay Two\"");

        let mut restored = Config::default();
        restored.register(&HOSTNAME).unwrap();
        restored.load_toml(&saved).unwrap();
        assert_eq!(restored.get_text(&HOSTNAME), "Relay Two");
        assert!(matches!(
            restored.load_toml("not toml ["),
            Err(ConfigError::Parse(_))
        ));
        let info = restored.info("game.hostname").unwrap();
        assert_eq!(info.default, &CVarValue::Text("Astrocore".into()));
        assert_eq!(restored.iter().count(), 1);
    }

    #[test]
    fn bad_file_value_falls_back_to_default() {
        let mut world = World::new();
        world.load_config("[net]\ntickrate = \"много\"\n").unwrap();
        world.register_cvar(&TICKRATE);
        assert_eq!(world.cvar(&TICKRATE), 30);
    }
}
