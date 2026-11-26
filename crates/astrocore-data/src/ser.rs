//! Преобразование типов Rust в дерево данных через `serde`.

use std::fmt;

use serde::ser::{self, Serialize};

use crate::de::{RAW_NODE, RAW_NODE_TOKEN};
use crate::node::{DataError, DataMap, DataNode, TYPE_TAG_PREFIX, Value};

impl ser::Error for DataError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        DataError::new(msg.to_string(), 0)
    }
}

/// Преобразует значение в дерево данных.
pub fn to_node<T: Serialize + ?Sized>(value: &T) -> Result<DataNode, DataError> {
    value.serialize(NodeSerializer)
}

impl Serialize for DataNode {
    fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RAW_NODE.with(|slot| *slot.borrow_mut() = Some(self.clone()));
        let result = serializer.serialize_newtype_struct(RAW_NODE_TOKEN, &());
        // Чужой сериализатор узел не забрал: не оставляем его следующему вызову.
        RAW_NODE.with(|slot| slot.borrow_mut().take());
        result
    }
}

fn tagged(node: DataNode, variant: &str) -> DataNode {
    node.with_tag(format!("{TYPE_TAG_PREFIX}{variant}"))
}

fn float_text(value: f64, single: bool) -> String {
    if value.is_nan() {
        return ".nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { ".inf" } else { "-.inf" }.into();
    }
    // Кратчайшая запись, которая читается обратно в то же число своей точности.
    if single {
        format!("{}", value as f32)
    } else {
        format!("{value}")
    }
}

struct NodeSerializer;

macro_rules! serialize_plain {
    ($($method:ident: $ty:ty),*) => {$(
        fn $method(self, v: $ty) -> Result<DataNode, DataError> {
            Ok(DataNode::plain(v.to_string()))
        }
    )*};
}

impl ser::Serializer for NodeSerializer {
    type Ok = DataNode;
    type Error = DataError;
    type SerializeSeq = SeqSer;
    type SerializeTuple = SeqSer;
    type SerializeTupleStruct = SeqSer;
    type SerializeTupleVariant = SeqSer;
    type SerializeMap = MapSer;
    type SerializeStruct = MapSer;
    type SerializeStructVariant = MapSer;

    serialize_plain! {
        serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32,
        serialize_i64: i64, serialize_i128: i128, serialize_u8: u8, serialize_u16: u16,
        serialize_u32: u32, serialize_u64: u64, serialize_u128: u128
    }

    fn serialize_f32(self, v: f32) -> Result<DataNode, DataError> {
        Ok(DataNode::plain(float_text(f64::from(v), true)))
    }

    fn serialize_f64(self, v: f64) -> Result<DataNode, DataError> {
        Ok(DataNode::plain(float_text(v, false)))
    }

    fn serialize_char(self, v: char) -> Result<DataNode, DataError> {
        Ok(DataNode::string(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<DataNode, DataError> {
        Ok(DataNode::string(v))
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<DataNode, DataError> {
        Err(DataError::new(
            "байтовые строки в дереве данных не поддержаны",
            0,
        ))
    }

    fn serialize_none(self) -> Result<DataNode, DataError> {
        Ok(DataNode::null())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<DataNode, DataError> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<DataNode, DataError> {
        Ok(DataNode::null())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<DataNode, DataError> {
        Ok(DataNode::null())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<DataNode, DataError> {
        Ok(DataNode::plain(variant))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<DataNode, DataError> {
        if name == RAW_NODE_TOKEN
            && let Some(node) = RAW_NODE.with(|slot| slot.borrow_mut().take())
        {
            return Ok(node);
        }
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<DataNode, DataError> {
        Ok(tagged(value.serialize(self)?, variant))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSer, DataError> {
        Ok(SeqSer {
            items: Vec::with_capacity(len.unwrap_or(0)),
            variant: None,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqSer, DataError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqSer, DataError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<SeqSer, DataError> {
        Ok(SeqSer {
            items: Vec::with_capacity(len),
            variant: Some(variant),
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<MapSer, DataError> {
        Ok(MapSer {
            map: DataMap(Vec::with_capacity(len.unwrap_or(0))),
            key: None,
            variant: None,
        })
    }

    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<MapSer, DataError> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<MapSer, DataError> {
        Ok(MapSer {
            map: DataMap(Vec::with_capacity(len)),
            key: None,
            variant: Some(variant),
        })
    }
}

struct SeqSer {
    items: Vec<DataNode>,
    variant: Option<&'static str>,
}

impl SeqSer {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), DataError> {
        self.items.push(value.serialize(NodeSerializer)?);
        Ok(())
    }

    fn finish(self) -> DataNode {
        let node = DataNode::seq(self.items);
        match self.variant {
            Some(variant) => tagged(node, variant),
            None => node,
        }
    }
}

macro_rules! impl_seq_ser {
    ($($trait:ident: $method:ident),*) => {$(
        impl ser::$trait for SeqSer {
            type Ok = DataNode;
            type Error = DataError;

            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), DataError> {
                self.push(value)
            }

            fn end(self) -> Result<DataNode, DataError> {
                Ok(self.finish())
            }
        }
    )*};
}

impl_seq_ser! {
    SerializeSeq: serialize_element, SerializeTuple: serialize_element,
    SerializeTupleStruct: serialize_field, SerializeTupleVariant: serialize_field
}

struct MapSer {
    map: DataMap,
    key: Option<String>,
    variant: Option<&'static str>,
}

impl MapSer {
    fn field<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), DataError> {
        self.map
            .0
            .push((key.to_owned(), value.serialize(NodeSerializer)?));
        Ok(())
    }

    fn finish(self) -> DataNode {
        let node = DataNode::map(self.map);
        match self.variant {
            Some(variant) => tagged(node, variant),
            None => node,
        }
    }
}

impl ser::SerializeMap for MapSer {
    type Ok = DataNode;
    type Error = DataError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), DataError> {
        let node = key.serialize(NodeSerializer)?;
        match node.value {
            Value::Scalar(scalar) => {
                self.key = Some(scalar.text);
                Ok(())
            }
            _ => Err(DataError::new("ключ отображения должен быть скаляром", 0)),
        }
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), DataError> {
        let key = self
            .key
            .take()
            .ok_or_else(|| DataError::new("значение записано раньше ключа", 0))?;
        self.field(&key, value)
    }

    fn end(self) -> Result<DataNode, DataError> {
        Ok(self.finish())
    }
}

macro_rules! impl_struct_ser {
    ($($trait:ident),*) => {$(
        impl ser::$trait for MapSer {
            type Ok = DataNode;
            type Error = DataError;

            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), DataError> {
                self.field(key, value)
            }

            fn end(self) -> Result<DataNode, DataError> {
                Ok(self.finish())
            }
        }
    )*};
}

impl_struct_ser!(SerializeStruct, SerializeStructVariant);
