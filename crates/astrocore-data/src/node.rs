//! Дерево данных: общий вид документов YAML и RON до разбора в типы Rust.
//!
//! Образец — `DataNode` Robust (`Robust.Shared/Serialization/Markdown` @ 1afd692): скаляр хранит
//! исходный текст, тип значения определяет получатель.

use std::fmt;

/// Префикс тега полиморфного значения: `!type:Foo` в YAML, `Foo(...)` в RON.
pub const TYPE_TAG_PREFIX: &str = "type:";

/// Узел дерева данных.
#[derive(Debug, Clone)]
pub struct DataNode {
    pub value: Value,
    /// Тег без ведущего `!`, например `type:PhysShapeCircle`.
    pub tag: Option<String>,
    /// Строка в исходном файле (с 1); 0 — узел создан программно.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Scalar(Scalar),
    Seq(Vec<DataNode>),
    Map(DataMap),
}

/// Скаляр: исходный текст и признак кавычек. Скаляр в кавычках — всегда строка;
/// скаляр без кавычек может быть числом, булевым значением, `null` или именем варианта.
#[derive(Debug, Clone, PartialEq)]
pub struct Scalar {
    pub text: String,
    pub quoted: bool,
}

/// Отображение с сохранением порядка ключей.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataMap(pub Vec<(String, DataNode)>);

impl PartialEq for DataNode {
    /// Номер строки в сравнении не участвует.
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag && self.value == other.value
    }
}

impl DataMap {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&DataNode> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut DataNode> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Вставляет значение; существующий ключ заменяется на прежнем месте.
    pub fn insert(&mut self, key: impl Into<String>, value: DataNode) -> Option<DataNode> {
        let key = key.into();
        match self.get_mut(&key) {
            Some(slot) => Some(std::mem::replace(slot, value)),
            None => {
                self.0.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<DataNode> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(index).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &DataNode)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }
}

impl FromIterator<(String, DataNode)> for DataMap {
    fn from_iter<T: IntoIterator<Item = (String, DataNode)>>(iter: T) -> Self {
        let mut map = DataMap::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl DataNode {
    pub fn new(value: Value) -> Self {
        Self {
            value,
            tag: None,
            line: 0,
        }
    }

    pub fn null() -> Self {
        Self::plain("null")
    }

    /// Скаляр без кавычек: число, булево значение, `null` или имя варианта.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(Value::Scalar(Scalar {
            text: text.into(),
            quoted: false,
        }))
    }

    /// Строка.
    pub fn string(text: impl Into<String>) -> Self {
        Self::new(Value::Scalar(Scalar {
            text: text.into(),
            quoted: true,
        }))
    }

    pub fn seq(items: Vec<DataNode>) -> Self {
        Self::new(Value::Seq(items))
    }

    pub fn map(map: DataMap) -> Self {
        Self::new(Value::Map(map))
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.line = line;
        self
    }

    /// Пустое значение: `null`, `~` или пустой скаляр без кавычек.
    pub fn is_null(&self) -> bool {
        match &self.value {
            Value::Scalar(s) => !s.quoted && matches!(s.text.as_str(), "" | "~" | "null" | "Null"),
            _ => false,
        }
    }

    pub fn as_scalar(&self) -> Option<&Scalar> {
        match &self.value {
            Value::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// Текст скаляра; `None` для последовательности, отображения и пустого значения.
    pub fn as_str(&self) -> Option<&str> {
        if self.is_null() {
            return None;
        }
        self.as_scalar().map(|s| s.text.as_str())
    }

    pub fn as_seq(&self) -> Option<&[DataNode]> {
        match &self.value {
            Value::Seq(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&DataMap> {
        match &self.value {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_map_mut(&mut self) -> Option<&mut DataMap> {
        match &mut self.value {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }

    /// Значение по ключу отображения.
    pub fn get(&self, key: &str) -> Option<&DataNode> {
        self.as_map()?.get(key)
    }

    /// Имя типа полиморфного значения: тег `type:Foo` даёт `Foo`.
    pub fn type_tag(&self) -> Option<&str> {
        self.tag.as_deref()?.strip_prefix(TYPE_TAG_PREFIX)
    }

    /// Имя вида записи: тег типа (`Foo(...)` в RON) или ключ `type` (YAML SS14).
    pub fn type_name(&self) -> Option<&str> {
        self.type_tag()
            .or_else(|| self.get("type").and_then(DataNode::as_str))
    }

    pub fn kind(&self) -> &'static str {
        match &self.value {
            Value::Scalar(_) => "скаляр",
            Value::Seq(_) => "последовательность",
            Value::Map(_) => "отображение",
        }
    }
}

/// Ошибка разбора или преобразования данных.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub struct DataError {
    pub message: String,
    /// Строка в исходном файле; 0 — неизвестна.
    pub line: u32,
}

impl DataError {
    pub fn new(message: impl Into<String>, line: u32) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "строка {}: {}", self.line, self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}
