//! Разбор дерева данных в типы Rust через `serde`.
//!
//! Правила как у сериализации Robust: скаляр хранит текст, тип выбирает получатель;
//! тег `type:Foo` выбирает вариант перечисления; пустое значение — `None`, пустой список
//! или структура со значениями по умолчанию.

use std::cell::RefCell;
use std::fmt;

use serde::de::{
    self, DeserializeOwned, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};

use crate::node::{DataError, DataMap, DataNode, Scalar, Value};

/// Имя-маркер: получатель `DataNode` просит узел целиком, без преобразования.
pub(crate) const RAW_NODE_TOKEN: &str = "$astrocore::DataNode";

thread_local! {
    /// Узел, переданный получателю `DataNode` в обход модели данных serde.
    pub(crate) static RAW_NODE: RefCell<Option<DataNode>> = const { RefCell::new(None) };
}

impl de::Error for DataError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        DataError::new(msg.to_string(), 0)
    }
}

/// Поле данных, которого нет у получателя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownField {
    pub name: String,
    pub line: u32,
}

/// Разбирает узел в значение типа `T`. Неизвестные поля игнорируются.
pub fn from_node<T: DeserializeOwned>(node: &DataNode) -> Result<T, DataError> {
    T::deserialize(NodeDeserializer {
        node,
        unknown: None,
    })
    .map_err(|e| locate(e, node))
}

/// Разбирает узел и возвращает поля, которые получатель не знает.
pub fn from_node_checked<T: DeserializeOwned>(
    node: &DataNode,
) -> Result<(T, Vec<UnknownField>), DataError> {
    let unknown = RefCell::new(Vec::new());
    let value = T::deserialize(NodeDeserializer {
        node,
        unknown: Some(&unknown),
    })
    .map_err(|e| locate(e, node))?;
    Ok((value, unknown.into_inner()))
}

fn locate(mut error: DataError, node: &DataNode) -> DataError {
    if error.line == 0 {
        error.line = node.line;
    }
    error
}

#[derive(Clone, Copy)]
struct NodeDeserializer<'a> {
    node: &'a DataNode,
    unknown: Option<&'a RefCell<Vec<UnknownField>>>,
}

/// Значение скаляра без кавычек, если получатель не назвал тип.
enum Plain<'a> {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(&'a str),
}

fn parse_float(text: &str) -> Option<f64> {
    match text {
        ".inf" | "+.inf" | "inf" | "+inf" => return Some(f64::INFINITY),
        "-.inf" | "-inf" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | "NaN" => return Some(f64::NAN),
        _ => {}
    }
    // `f64::from_str` принимает слова `infinity` и `nan`; в данных это строки.
    let first = text.trim_start_matches(['-', '+']).bytes().next()?;
    if !(first.is_ascii_digit() || first == b'.') {
        return None;
    }
    text.parse().ok()
}

fn parse_int<T: TryFrom<i128>>(text: &str) -> Option<T> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let value = if let Some(hex) = digits.strip_prefix("0x") {
        i128::from_str_radix(hex, 16).ok()?
    } else if let Some(bin) = digits.strip_prefix("0b") {
        i128::from_str_radix(bin, 2).ok()?
    } else if let Some(oct) = digits.strip_prefix("0o") {
        i128::from_str_radix(oct, 8).ok()?
    } else {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()?
    };
    T::try_from(if negative { -value } else { value }).ok()
}

fn classify(scalar: &Scalar) -> Plain<'_> {
    let text = scalar.text.as_str();
    if scalar.quoted {
        return Plain::Str(text);
    }
    match text {
        "" | "~" | "null" | "Null" => return Plain::Null,
        "true" | "True" => return Plain::Bool(true),
        "false" | "False" => return Plain::Bool(false),
        _ => {}
    }
    if let Some(v) = parse_int::<i64>(text) {
        return Plain::Int(v);
    }
    if let Some(v) = parse_int::<u64>(text) {
        return Plain::UInt(v);
    }
    match parse_float(text) {
        Some(v) => Plain::Float(v),
        None => Plain::Str(text),
    }
}

/// Имя варианта, как оно объявлено в типе: данные SS14 пишут варианты и без учёта регистра
/// (`bodyType: static`), как это разрешает Robust.
fn canonical_variant<'a>(text: &'a str, variants: &'static [&'static str]) -> &'a str {
    if variants.contains(&text) {
        return text;
    }
    variants
        .iter()
        .find(|variant| variant.eq_ignore_ascii_case(text))
        .copied()
        .unwrap_or(text)
}

impl<'a> NodeDeserializer<'a> {
    fn child(self, node: &'a DataNode) -> Self {
        Self {
            node,
            unknown: self.unknown,
        }
    }

    fn error(self, expected: &str) -> DataError {
        let found = match &self.node.value {
            Value::Scalar(s) => format!("`{}`", s.text),
            _ => self.node.kind().to_owned(),
        };
        DataError::new(
            format!("ожидалось {expected}, получено {found}"),
            self.node.line,
        )
    }

    fn scalar(self, expected: &str) -> Result<&'a Scalar, DataError> {
        self.node.as_scalar().ok_or_else(|| self.error(expected))
    }

    fn int<T: TryFrom<i128>>(self) -> Result<T, DataError> {
        let scalar = self.scalar("целое число")?;
        parse_int(scalar.text.trim()).ok_or_else(|| self.error("целое число"))
    }

    fn float(self) -> Result<f64, DataError> {
        let scalar = self.scalar("число")?;
        parse_float(scalar.text.trim()).ok_or_else(|| self.error("число"))
    }

    fn seq<V: Visitor<'a>>(self, items: &'a [DataNode], visitor: V) -> Result<V::Value, DataError> {
        let mut access = SeqDe {
            parent: self,
            items: items.iter(),
        };
        let value = visitor.visit_seq(&mut access)?;
        match access.items.len() {
            0 => Ok(value),
            extra => Err(DataError::new(
                format!("лишних элементов в последовательности: {extra}"),
                self.node.line,
            )),
        }
    }

    fn map<V: Visitor<'a>>(self, map: &'a DataMap, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_map(MapDe {
            parent: self,
            entries: map.0.iter(),
            current: None,
        })
    }
}

macro_rules! deserialize_int {
    ($($method:ident => $visit:ident),*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
            visitor.$visit(self.int()?)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for NodeDeserializer<'de> {
    type Error = DataError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        match &self.node.value {
            Value::Scalar(scalar) => match classify(scalar) {
                Plain::Null => visitor.visit_unit(),
                Plain::Bool(v) => visitor.visit_bool(v),
                Plain::Int(v) => visitor.visit_i64(v),
                Plain::UInt(v) => visitor.visit_u64(v),
                Plain::Float(v) => visitor.visit_f64(v),
                Plain::Str(v) => visitor.visit_borrowed_str(v),
            },
            Value::Seq(items) => self.seq(items, visitor),
            Value::Map(map) => self.map(map, visitor),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        match self.scalar("true или false")?.text.trim() {
            "true" | "True" | "TRUE" => visitor.visit_bool(true),
            "false" | "False" | "FALSE" => visitor.visit_bool(false),
            _ => Err(self.error("true или false")),
        }
    }

    deserialize_int! {
        deserialize_i8 => visit_i8, deserialize_i16 => visit_i16, deserialize_i32 => visit_i32,
        deserialize_i64 => visit_i64, deserialize_i128 => visit_i128,
        deserialize_u8 => visit_u8, deserialize_u16 => visit_u16, deserialize_u32 => visit_u32,
        deserialize_u64 => visit_u64, deserialize_u128 => visit_u128
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_f32(self.float()? as f32)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_f64(self.float()?)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        let text = &self.scalar("символ")?.text;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(ch), None) => visitor.visit_char(ch),
            _ => Err(self.error("символ")),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_borrowed_str(&self.scalar("строка")?.text)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_borrowed_bytes(self.scalar("строка байтов")?.text.as_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        if self.node.is_null() && self.node.tag.is_none() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        match &self.node.value {
            Value::Map(map) if map.is_empty() => visitor.visit_unit(),
            _ if self.node.is_null() => visitor.visit_unit(),
            _ => Err(self.error("пустое значение")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        if name == RAW_NODE_TOKEN {
            RAW_NODE.with(|slot| *slot.borrow_mut() = Some(self.node.clone()));
            return visitor.visit_unit();
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        match &self.node.value {
            Value::Seq(items) => self.seq(items, visitor),
            _ if self.node.is_null() => self.seq(&[], visitor),
            _ => Err(self.error("последовательность")),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        match &self.node.value {
            Value::Seq(items) => self.seq(items, visitor),
            // Запись SS14 для векторов и прямоугольников: `1,2` и `0,0,1,1`.
            Value::Scalar(scalar) if scalar.text.contains(',') => {
                let line = self.node.line;
                let parts: Vec<DataNode> = scalar
                    .text
                    .split(',')
                    .map(|part| DataNode::plain(part.trim()).with_line(line))
                    .collect();
                let mut access = OwnedSeq {
                    items: parts.into_iter(),
                };
                let value = visitor.visit_seq(&mut access)?;
                match access.items.len() {
                    0 => Ok(value),
                    _ => Err(self.error("кортеж меньшей длины")),
                }
            }
            _ => Err(self.error("кортеж")),
        }
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        const EMPTY: &DataMap = &DataMap(Vec::new());
        match &self.node.value {
            Value::Map(map) => self.map(map, visitor),
            _ if self.node.is_null() => self.map(EMPTY, visitor),
            _ => Err(self.error("отображение")),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        match &self.node.value {
            Value::Seq(items) => self.seq(items, visitor),
            _ => self.deserialize_map(visitor),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        // `!type:Foo {..}` в YAML и `Foo(..)` в RON.
        if let Some(variant) = self.node.type_tag() {
            return visitor.visit_enum(EnumDe {
                variant: canonical_variant(variant, variants),
                content: Some(self),
            });
        }
        match &self.node.value {
            Value::Scalar(scalar) if !self.node.is_null() => visitor.visit_enum(EnumDe {
                variant: canonical_variant(&scalar.text, variants),
                content: None,
            }),
            // Запись serde: `{Variant: содержимое}`.
            Value::Map(map) if map.len() == 1 => {
                let (variant, content) = &map.0[0];
                visitor.visit_enum(EnumDe {
                    variant: canonical_variant(variant, variants),
                    content: Some(self.child(content)),
                })
            }
            _ => Err(self.error("вариант перечисления")),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        visitor.visit_unit()
    }
}

struct SeqDe<'a> {
    parent: NodeDeserializer<'a>,
    items: std::slice::Iter<'a, DataNode>,
}

impl<'de> SeqAccess<'de> for &mut SeqDe<'de> {
    type Error = DataError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, DataError> {
        match self.items.next() {
            Some(node) => seed
                .deserialize(self.parent.child(node))
                .map(Some)
                .map_err(|e| locate(e, node)),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

/// Элементы, собранные из скаляра вида `1,2`.
struct OwnedSeq {
    items: std::vec::IntoIter<DataNode>,
}

impl<'de> SeqAccess<'de> for &mut OwnedSeq {
    type Error = DataError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, DataError> {
        match self.items.next() {
            Some(node) => seed.deserialize(OwnedScalar(node)).map(Some),
            None => Ok(None),
        }
    }
}

/// Скаляр, которым владеет десериализатор: часть составного скаляра или ключ отображения.
struct OwnedScalar(DataNode);

impl<'de> de::Deserializer<'de> for OwnedScalar {
    type Error = DataError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        let Value::Scalar(scalar) = &self.0.value else {
            return Err(DataError::new("ожидался скаляр", self.0.line));
        };
        let result = match classify(scalar) {
            Plain::Null => visitor.visit_unit(),
            Plain::Bool(v) => visitor.visit_bool(v),
            Plain::Int(v) => visitor.visit_i64(v),
            Plain::UInt(v) => visitor.visit_u64(v),
            Plain::Float(v) => visitor.visit_f64(v),
            Plain::Str(v) => visitor.visit_str(v),
        };
        result.map_err(|e| locate(e, &self.0))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        match &self.0.value {
            Value::Scalar(scalar) => visitor.visit_str(&scalar.text),
            _ => Err(DataError::new("ожидался скаляр", self.0.line)),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        let text = self.0.as_scalar().map_or("", |s| s.text.trim());
        match parse_float(text) {
            Some(v) => visitor.visit_f64(v),
            None => Err(DataError::new(
                format!("ожидалось число, получено `{text}`"),
                self.0.line,
            )),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        match &self.0.value {
            Value::Scalar(scalar) => visitor.visit_enum(scalar.text.as_str().into_deserializer()),
            _ => Err(DataError::new("ожидался скаляр", self.0.line)),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        if self.0.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        visitor.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 char bytes byte_buf unit unit_struct
        seq tuple tuple_struct map struct ignored_any
    }
}

struct MapDe<'a> {
    parent: NodeDeserializer<'a>,
    entries: std::slice::Iter<'a, (String, DataNode)>,
    current: Option<&'a (String, DataNode)>,
}

impl<'de> MapAccess<'de> for MapDe<'de> {
    type Error = DataError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, DataError> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        self.current = Some(entry);
        let key = DataNode::plain(entry.0.as_str()).with_line(entry.1.line);
        seed.deserialize(OwnedScalar(key)).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, DataError> {
        let (key, node) = self
            .current
            .take()
            .ok_or_else(|| DataError::new("значение запрошено раньше ключа", 0))?;
        seed.deserialize(ValueDe {
            inner: self.parent.child(node),
            key,
        })
        .map_err(|e| locate(e, node))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// Значение поля: запоминает ключ, если получатель его пропускает как неизвестный.
struct ValueDe<'a> {
    inner: NodeDeserializer<'a>,
    key: &'a str,
}

macro_rules! forward_value {
    ($($method:ident),*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
            self.inner.$method(visitor)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for ValueDe<'de> {
    type Error = DataError;

    forward_value! {
        deserialize_any, deserialize_bool, deserialize_i8, deserialize_i16, deserialize_i32,
        deserialize_i64, deserialize_i128, deserialize_u8, deserialize_u16, deserialize_u32,
        deserialize_u64, deserialize_u128, deserialize_f32, deserialize_f64, deserialize_char,
        deserialize_str, deserialize_string, deserialize_bytes, deserialize_byte_buf,
        deserialize_option, deserialize_unit, deserialize_seq, deserialize_map,
        deserialize_identifier
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        if let Some(unknown) = self.inner.unknown {
            unknown.borrow_mut().push(UnknownField {
                name: self.key.to_owned(),
                line: self.inner.node.line,
            });
        }
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.inner.deserialize_enum(name, variants, visitor)
    }
}

struct EnumDe<'a> {
    variant: &'a str,
    content: Option<NodeDeserializer<'a>>,
}

impl<'de> EnumAccess<'de> for EnumDe<'de> {
    type Error = DataError;
    type Variant = VariantDe<'de>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, VariantDe<'de>), DataError> {
        let line = self.content.map_or(0, |c| c.node.line);
        let variant = seed
            .deserialize(OwnedScalar(DataNode::plain(self.variant).with_line(line)))
            .map_err(|mut e: DataError| {
                if e.line == 0 {
                    e.line = line;
                }
                e
            })?;
        Ok((variant, VariantDe(self.content)))
    }
}

struct VariantDe<'a>(Option<NodeDeserializer<'a>>);

impl<'de> VariantAccess<'de> for VariantDe<'de> {
    type Error = DataError;

    fn unit_variant(self) -> Result<(), DataError> {
        match self.0 {
            None => Ok(()),
            Some(content) => match &content.node.value {
                Value::Map(map) if map.is_empty() => Ok(()),
                _ if content.node.is_null() => Ok(()),
                _ => Err(content.error("вариант без содержимого")),
            },
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, DataError> {
        match self.0 {
            // Тег уже выбрал вариант: внутрь передаём тот же узел без тега.
            Some(content) => seed.deserialize(Untagged(content)),
            None => Err(DataError::new("у варианта нет содержимого", 0)),
        }
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, DataError> {
        let content = self
            .0
            .ok_or_else(|| DataError::new("у варианта нет содержимого", 0))?;
        match &content.node.value {
            Value::Seq(items) => content.seq(items, visitor),
            _ if len == 1 => content.seq(std::slice::from_ref(content.node), visitor),
            _ => Err(content.error("кортеж")),
        }
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        const EMPTY: &DataMap = &DataMap(Vec::new());
        match self.0 {
            Some(content) => match &content.node.value {
                Value::Map(map) => content.map(map, visitor),
                Value::Seq(items) => content.seq(items, visitor),
                _ if content.node.is_null() => content.map(EMPTY, visitor),
                _ => Err(content.error("поля варианта")),
            },
            None => Err(DataError::new("у варианта нет содержимого", 0)),
        }
    }
}

/// Содержимое варианта: тот же узел, но тег при разборе вложенного перечисления не учитывается.
struct Untagged<'a>(NodeDeserializer<'a>);

macro_rules! forward_untagged {
    ($($method:ident),*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
            self.0.$method(visitor)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Untagged<'de> {
    type Error = DataError;

    forward_untagged! {
        deserialize_any, deserialize_bool, deserialize_i8, deserialize_i16, deserialize_i32,
        deserialize_i64, deserialize_i128, deserialize_u8, deserialize_u16, deserialize_u32,
        deserialize_u64, deserialize_u128, deserialize_f32, deserialize_f64, deserialize_char,
        deserialize_str, deserialize_string, deserialize_bytes, deserialize_byte_buf,
        deserialize_unit, deserialize_seq, deserialize_map, deserialize_identifier,
        deserialize_ignored_any
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DataError> {
        if self.0.node.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.0.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        if name == RAW_NODE_TOKEN {
            let mut node = self.0.node.clone();
            node.tag = None;
            RAW_NODE.with(|slot| *slot.borrow_mut() = Some(node));
            return visitor.visit_unit();
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.0.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.0.deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        self.0.deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DataError> {
        let node = self.0.node;
        match &node.value {
            Value::Scalar(scalar) if !node.is_null() => visitor.visit_enum(EnumDe {
                variant: &scalar.text,
                content: None,
            }),
            Value::Map(map) if map.len() == 1 => {
                let (variant, content) = &map.0[0];
                visitor.visit_enum(EnumDe {
                    variant,
                    content: Some(self.0.child(content)),
                })
            }
            _ => Err(self.0.error("вариант перечисления")),
        }
    }
}

impl<'de> de::Deserialize<'de> for DataNode {
    /// Из дерева данных узел берётся целиком, вместе с тегами и номерами строк.
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RawVisitor;

        impl<'de> Visitor<'de> for RawVisitor {
            type Value = DataNode;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("узел дерева данных")
            }

            fn visit_unit<E: de::Error>(self) -> Result<DataNode, E> {
                RAW_NODE
                    .with(|slot| slot.borrow_mut().take())
                    .ok_or_else(|| E::custom("DataNode читается только из дерева данных Astrocore"))
            }
        }

        deserializer.deserialize_newtype_struct(RAW_NODE_TOKEN, RawVisitor)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;
    use crate::{parse_ron, parse_yaml};

    #[derive(Debug, Deserialize, PartialEq, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Fixture {
        shape: Option<Shape>,
        density: f32,
        hard: bool,
        layer: Vec<String>,
        offset: (f32, f32),
        base_speed: u8,
        raw: Option<DataNode>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    enum Shape {
        PhysShapeCircle { radius: f32 },
        PhysShapeAabb { bounds: (f32, f32, f32, f32) },
        Point,
        Scaled(f32),
    }

    #[derive(Debug, Deserialize, PartialEq)]
    enum Size {
        Tiny,
        Small,
    }

    #[test]
    fn struct_from_yaml_with_tag_and_scalar_forms() {
        let node = parse_yaml(
            "shape: !type:PhysShapeCircle\n  radius: 0.35\ndensity: 20\nhard: true\nlayer: [A, B]\noffset: 0.5,-1\nbaseSpeed: 0x10\n",
        )
        .unwrap();
        let fixture: Fixture = from_node(&node).unwrap();
        assert_eq!(
            fixture,
            Fixture {
                shape: Some(Shape::PhysShapeCircle { radius: 0.35 }),
                density: 20.0,
                hard: true,
                layer: vec!["A".into(), "B".into()],
                offset: (0.5, -1.0),
                base_speed: 16,
                raw: None,
            }
        );
    }

    #[test]
    fn same_struct_from_ron() {
        let node = parse_ron(
            "(shape: PhysShapeAabb(bounds: (0, 0, 1, 1)), density: 1.5, raw: Thing(a: 1), layer: [])",
        )
        .unwrap();
        let fixture: Fixture = from_node(&node).unwrap();
        assert_eq!(
            fixture.shape,
            Some(Shape::PhysShapeAabb {
                bounds: (0.0, 0.0, 1.0, 1.0)
            })
        );
        let raw = fixture.raw.unwrap();
        assert_eq!(raw.type_tag(), Some("Thing"));
        assert_eq!(raw.get("a").unwrap().as_str(), Some("1"));
    }

    #[test]
    fn enum_forms() {
        assert_eq!(
            from_node::<Size>(&parse_yaml("Small").unwrap()).unwrap(),
            Size::Small
        );
        // Регистр имени варианта не важен, как в Robust.
        assert_eq!(
            from_node::<Size>(&parse_yaml("small").unwrap()).unwrap(),
            Size::Small
        );
        assert_eq!(
            from_node::<Shape>(&parse_yaml("!type:point").unwrap()).unwrap(),
            Shape::Point
        );
        assert_eq!(
            from_node::<Shape>(&parse_yaml("!type:Point").unwrap()).unwrap(),
            Shape::Point
        );
        assert_eq!(
            from_node::<Shape>(&parse_ron("Point").unwrap()).unwrap(),
            Shape::Point
        );
        assert_eq!(
            from_node::<Shape>(&parse_ron("Scaled(2.5)").unwrap()).unwrap(),
            Shape::Scaled(2.5)
        );
        assert_eq!(
            from_node::<Shape>(&parse_yaml("!type:Scaled 2.5").unwrap()).unwrap(),
            Shape::Scaled(2.5)
        );
        assert_eq!(
            from_node::<Vec<Shape>>(&parse_yaml("- !type:Point\n- Scaled: 1\n").unwrap()).unwrap(),
            vec![Shape::Point, Shape::Scaled(1.0)]
        );
    }

    #[test]
    fn maps_with_typed_keys_and_untyped_values() {
        let node = parse_yaml("1: a\n2: \"3\"\n").unwrap();
        let map: BTreeMap<u32, String> = from_node(&node).unwrap();
        assert_eq!(map[&2], "3");

        #[derive(Deserialize, Debug, PartialEq)]
        #[serde(untagged)]
        enum Any {
            Bool(bool),
            Int(i64),
            Float(f64),
            Text(String),
        }
        let values: Vec<Any> =
            from_node(&parse_yaml("[true, 7, 1.5, text, \"7\", 007x]").unwrap()).unwrap();
        assert_eq!(
            values,
            [
                Any::Bool(true),
                Any::Int(7),
                Any::Float(1.5),
                Any::Text("text".into()),
                Any::Text("7".into()),
                Any::Text("007x".into()),
            ]
        );
    }

    #[test]
    fn reports_unknown_fields_and_error_lines() {
        let node = parse_yaml(
            "density: 1\nmass: 5\nshape: !type:PhysShapeCircle\n  radius: 1\n  color: red\n",
        )
        .unwrap();
        let (_, unknown) = from_node_checked::<Fixture>(&node).unwrap();
        assert_eq!(
            unknown,
            [
                UnknownField {
                    name: "mass".into(),
                    line: 2
                },
                UnknownField {
                    name: "color".into(),
                    line: 5
                }
            ]
        );

        let error =
            from_node::<Fixture>(&parse_yaml("density: 1\nhard: maybe\n").unwrap()).unwrap_err();
        assert_eq!(error.line, 2);
        let error =
            from_node::<Fixture>(&parse_yaml("shape: !type:Triangle {}\n").unwrap()).unwrap_err();
        assert!(error.message.contains("Triangle"), "{error}");
    }

    #[test]
    fn null_means_default_collections() {
        #[derive(Deserialize, Default, Debug, PartialEq)]
        struct Holder {
            list: Vec<u8>,
            map: BTreeMap<String, u8>,
            inner: Option<Box<Holder>>,
        }
        let holder: Holder = from_node(&parse_yaml("list:\nmap:\ninner:\n").unwrap()).unwrap();
        assert_eq!(holder, Holder::default());
    }
}
