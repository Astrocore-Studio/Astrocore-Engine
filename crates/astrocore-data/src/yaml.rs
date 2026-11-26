//! Разбор YAML в дерево данных. Формат — как в `Resources/` SS14: теги `!type:Foo`,
//! скаляры без приведения типов, якоря и ссылки.

use std::borrow::Cow;
use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, Span, SpannedEventReceiver, Tag};

use crate::node::{DataError, DataMap, DataNode, Scalar, Value};

/// Разбирает первый документ YAML. Пустой файл даёт пустое значение.
pub fn parse_yaml(text: &str) -> Result<DataNode, DataError> {
    Ok(parse_yaml_documents(text)?
        .into_iter()
        .next()
        .unwrap_or_else(DataNode::null))
}

/// Разбирает все документы YAML из текста.
pub fn parse_yaml_documents(text: &str) -> Result<Vec<DataNode>, DataError> {
    // BOM в начале файла встречается в ресурсах SS14.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(text);
    parser.load(&mut builder, true).map_err(|error| {
        DataError::new(
            format!("YAML: {}", error.info()),
            error.marker().line() as u32,
        )
    })?;
    match builder.error {
        Some(error) => Err(error),
        None => Ok(builder.documents),
    }
}

enum Frame {
    Seq {
        items: Vec<DataNode>,
        tag: Option<String>,
        line: u32,
        anchor: usize,
    },
    Map {
        map: DataMap,
        key: Option<String>,
        tag: Option<String>,
        line: u32,
        anchor: usize,
    },
}

#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    anchors: HashMap<usize, DataNode>,
    documents: Vec<DataNode>,
    error: Option<DataError>,
}

fn tag_text(tag: Option<Cow<'_, Tag>>) -> Option<String> {
    let tag = tag?;
    // Стандартные теги YAML (`!!str`, `!!int`) тип получателя не меняют.
    if tag.handle.starts_with("tag:yaml.org") || tag.handle == "!!" {
        return None;
    }
    Some(tag.suffix.clone())
}

impl Builder {
    fn fail(&mut self, message: impl Into<String>, line: u32) {
        if self.error.is_none() {
            self.error = Some(DataError::new(message, line));
        }
    }

    fn complete(&mut self, node: DataNode, anchor: usize) {
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => self.documents.push(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map { map, key, .. }) => match key.take() {
                Some(key) => {
                    map.insert(key, node);
                }
                None => match &node.value {
                    Value::Scalar(scalar) => *key = Some(scalar.text.clone()),
                    _ => {
                        let line = node.line;
                        self.fail("ключ отображения должен быть скаляром", line);
                    }
                },
            },
        }
    }
}

impl<'input> SpannedEventReceiver<'input> for Builder {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        if self.error.is_some() {
            return;
        }
        let line = span.start.line() as u32;
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                let string_tag = tag
                    .as_deref()
                    .is_some_and(|t| t.suffix == "str" && t.handle.contains("yaml.org"));
                let node = DataNode {
                    value: Value::Scalar(Scalar {
                        text: text.into_owned(),
                        quoted: style != ScalarStyle::Plain || string_tag,
                    }),
                    tag: tag_text(tag),
                    line,
                };
                self.complete(node, anchor);
            }
            Event::SequenceStart(anchor, tag) => self.stack.push(Frame::Seq {
                items: Vec::new(),
                tag: tag_text(tag),
                line,
                anchor,
            }),
            Event::MappingStart(anchor, tag) => self.stack.push(Frame::Map {
                map: DataMap::new(),
                key: None,
                tag: tag_text(tag),
                line,
                anchor,
            }),
            Event::SequenceEnd | Event::MappingEnd => {
                let (value, tag, line, anchor) = match self.stack.pop() {
                    Some(Frame::Seq {
                        items,
                        tag,
                        line,
                        anchor,
                    }) => (Value::Seq(items), tag, line, anchor),
                    Some(Frame::Map {
                        map,
                        tag,
                        line,
                        anchor,
                        ..
                    }) => (Value::Map(map), tag, line, anchor),
                    None => return self.fail("лишнее закрытие коллекции", line),
                };
                self.complete(DataNode { value, tag, line }, anchor);
            }
            Event::Alias(anchor) => match self.anchors.get(&anchor).cloned() {
                Some(node) => self.complete(node, 0),
                None => self.fail("ссылка на неизвестный якорь", line),
            },
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart(_)
            | Event::DocumentEnd => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_scalar_text_tags_and_lines() {
        let node = parse_yaml(
            "- type: entity\n  id: 007\n  name: \"true\"\n  shape: !type:PhysShapeCircle\n    radius: 0.35\n  empty:\n",
        )
        .unwrap();
        let entity = &node.as_seq().unwrap()[0];
        assert_eq!(entity.type_name(), Some("entity"));
        let id = entity.get("id").unwrap().as_scalar().unwrap();
        assert_eq!((id.text.as_str(), id.quoted), ("007", false));
        assert!(entity.get("name").unwrap().as_scalar().unwrap().quoted);
        let shape = entity.get("shape").unwrap();
        assert_eq!(shape.type_tag(), Some("PhysShapeCircle"));
        assert_eq!(shape.line, 4);
        assert_eq!(shape.get("radius").unwrap().as_str(), Some("0.35"));
        assert!(entity.get("empty").unwrap().is_null());
    }

    #[test]
    fn tagged_scalar_and_empty_tagged_value() {
        let node = parse_yaml("a: !type:Foo\nb: !type:Bar {}\n").unwrap();
        assert_eq!(node.get("a").unwrap().type_tag(), Some("Foo"));
        assert!(node.get("a").unwrap().is_null());
        assert_eq!(node.get("b").unwrap().type_tag(), Some("Bar"));
    }

    #[test]
    fn resolves_anchors() {
        let node = parse_yaml("base: &b\n  x: 1\ncopy: *b\n").unwrap();
        assert_eq!(node.get("copy"), node.get("base"));
    }

    #[test]
    fn reports_error_line() {
        let error = parse_yaml("a: [1, 2\nb: 3\n").unwrap_err();
        assert!(error.line >= 1);
    }

    #[test]
    fn multiple_documents_and_bom() {
        let docs = parse_yaml_documents("\u{feff}a: 1\n---\nb: 2\n").unwrap();
        assert_eq!(docs.len(), 2);
    }
}
