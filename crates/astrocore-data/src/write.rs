//! Запись дерева данных в текст RON и YAML.
//!
//! YAML читается обратно в то же дерево. В RON строка без кавычек, не похожая на число или
//! идентификатор, получает кавычки: значение то же, но признак кавычек меняется.

use std::fmt::Write;

use crate::node::{DataMap, DataNode, Scalar, Value};

const INDENT: &str = "    ";

fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if let Some(hex) = digits.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    digits.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && digits.parse::<f64>().is_ok()
        && !digits.contains(['i', 'n', 'N'])
}

fn quote(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------- RON

/// Записывает дерево в RON с отступами.
pub fn to_ron_string(node: &DataNode) -> String {
    let mut out = String::new();
    ron_value(node, 0, &mut out);
    out.push('\n');
    out
}

fn ron_scalar(scalar: &Scalar, out: &mut String) {
    let text = scalar.text.as_str();
    if scalar.quoted {
        return quote(text, out);
    }
    match text {
        "" | "~" | "null" | "Null" => out.push_str("None"),
        "true" | "True" => out.push_str("true"),
        "false" | "False" => out.push_str("false"),
        ".inf" | "+.inf" => out.push_str("inf"),
        "-.inf" => out.push_str("-inf"),
        ".nan" | ".NaN" => out.push_str("NaN"),
        // Слова RON, которые как идентификатор означали бы другое.
        "None" | "Some" | "inf" | "NaN" => quote(text, out),
        _ if is_number(text) || is_identifier(text) => out.push_str(text),
        _ => quote(text, out),
    }
}

fn ron_newline(depth: usize, out: &mut String) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// Короткие последовательности скаляров пишутся в одну строку.
fn is_inline(items: &[DataNode]) -> bool {
    items.len() <= 8
        && items
            .iter()
            .all(|n| n.tag.is_none() && n.as_scalar().is_some_and(|s| s.text.len() <= 24))
}

fn ron_items(items: &[DataNode], depth: usize, open: char, close: char, out: &mut String) {
    out.push(open);
    if is_inline(items) {
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            ron_value(item, depth, out);
        }
    } else {
        for item in items {
            ron_newline(depth + 1, out);
            ron_value(item, depth + 1, out);
            out.push(',');
        }
        ron_newline(depth, out);
    }
    out.push(close);
}

fn ron_fields(map: &DataMap, depth: usize, out: &mut String) {
    // Поля структуры — идентификаторы; иначе это словарь с ключами-строками.
    let as_struct = map.keys().all(is_identifier);
    out.push(if as_struct { '(' } else { '{' });
    for (key, value) in map.iter() {
        ron_newline(depth + 1, out);
        if as_struct {
            out.push_str(key);
        } else {
            quote(key, out);
        }
        out.push_str(": ");
        ron_value(value, depth + 1, out);
        out.push(',');
    }
    if !map.is_empty() {
        ron_newline(depth, out);
    }
    out.push(if as_struct { ')' } else { '}' });
}

fn ron_value(node: &DataNode, depth: usize, out: &mut String) {
    let Some(name) = node.type_tag() else {
        match &node.value {
            Value::Scalar(scalar) => ron_scalar(scalar, out),
            Value::Seq(items) => ron_items(items, depth, '[', ']', out),
            Value::Map(map) => ron_fields(map, depth, out),
        }
        return;
    };
    out.push_str(name);
    match &node.value {
        Value::Map(map) if map.keys().all(is_identifier) => ron_fields(map, depth, out),
        Value::Map(map) => {
            out.push('(');
            ron_fields(map, depth, out);
            out.push(')');
        }
        // `Foo(a, b)` читается как последовательность только при двух и более элементах.
        Value::Seq(items) if items.len() >= 2 => ron_items(items, depth, '(', ')', out),
        Value::Seq(items) => {
            out.push('(');
            ron_items(items, depth, '[', ']', out);
            out.push(')');
        }
        Value::Scalar(_) if node.is_null() => out.push_str("()"),
        Value::Scalar(scalar) => {
            out.push('(');
            ron_scalar(scalar, out);
            out.push(')');
        }
    }
}

// ---------------------------------------------------------------- YAML

/// Записывает дерево в YAML блочным стилем, как файлы SS14.
pub fn to_yaml_string(node: &DataNode) -> String {
    let mut out = String::new();
    match &node.value {
        Value::Scalar(_) => {
            yaml_inline(node, &mut out);
            out.push('\n');
        }
        _ if yaml_is_inline(node) => {
            yaml_inline(node, &mut out);
            out.push('\n');
        }
        _ => {
            if let Some(tag) = &node.tag {
                let _ = writeln!(out, "!{tag}");
            }
            yaml_block(node, 0, &mut out);
        }
    }
    out
}

fn yaml_plain_safe(text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    if matches!(
        first,
        '-' | '?'
            | ':'
            | ','
            | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | '&'
            | '*'
            | '!'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
            | ' '
    ) && !(first == '-' && is_number(text))
    {
        return false;
    }
    !text.ends_with([' ', ':'])
        && !text.contains(": ")
        && !text.contains(" #")
        && !text.contains(['\n', '\r', '\t'])
}

fn yaml_scalar(scalar: &Scalar, out: &mut String) {
    let text = scalar.text.as_str();
    if scalar.quoted {
        quote(text, out);
    } else if text.is_empty() {
        out.push_str("null");
    } else if yaml_plain_safe(text) {
        out.push_str(text);
    } else {
        quote(text, out);
    }
}

fn yaml_is_inline(node: &DataNode) -> bool {
    match &node.value {
        Value::Scalar(_) => true,
        Value::Seq(items) => items.is_empty(),
        Value::Map(map) => map.is_empty(),
    }
}

fn yaml_inline(node: &DataNode, out: &mut String) {
    if let Some(tag) = &node.tag {
        let _ = write!(out, "!{tag}");
        if node.is_null() {
            return;
        }
        out.push(' ');
    }
    match &node.value {
        Value::Scalar(scalar) => yaml_scalar(scalar, out),
        Value::Seq(_) => out.push_str("[]"),
        Value::Map(_) => out.push_str("{}"),
    }
}

fn yaml_indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn yaml_key(key: &str, out: &mut String) {
    yaml_scalar(
        &Scalar {
            text: key.to_owned(),
            quoted: false,
        },
        out,
    );
}

/// Пишет непустую коллекцию; курсор стоит в начале строки.
fn yaml_block(node: &DataNode, depth: usize, out: &mut String) {
    match &node.value {
        Value::Scalar(_) => {}
        Value::Seq(items) => {
            for item in items {
                yaml_indent(depth, out);
                out.push('-');
                yaml_entry(item, depth, true, out);
            }
        }
        Value::Map(map) => yaml_map(map, depth, false, out),
    }
}

/// Пишет поля отображения. `first_inline` — первое поле продолжает строку после `- `.
fn yaml_map(map: &DataMap, depth: usize, first_inline: bool, out: &mut String) {
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 || !first_inline {
            yaml_indent(depth, out);
        }
        yaml_key(key, out);
        out.push(':');
        yaml_entry(value, depth, false, out);
    }
}

/// Пишет значение после `key:` или `-` и завершает строку.
fn yaml_entry(node: &DataNode, depth: usize, in_seq: bool, out: &mut String) {
    if yaml_is_inline(node) {
        out.push(' ');
        yaml_inline(node, out);
        out.push('\n');
        return;
    }
    if let Some(tag) = &node.tag {
        let _ = write!(out, " !{tag}");
    }
    match &node.value {
        // `- key: value` — первое поле на строке элемента.
        Value::Map(map) if in_seq && node.tag.is_none() => {
            out.push(' ');
            yaml_map(map, depth + 1, true, out);
        }
        _ => {
            out.push('\n');
            // Последовательность под ключом отображения пишется без отступа, как в SS14.
            let child = match (&node.value, in_seq) {
                (Value::Seq(_), false) => depth,
                _ => depth + 1,
            };
            yaml_block(node, child, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{from_node, parse_ron, parse_yaml, to_node};

    const SOURCE: &str = r##"
- type: entity
  id: Crowbar
  parent: [BaseItem, BaseTool]
  name: "crowbar: red"
  description: null
  components:
  - type: Sprite
    sprite: Objects/Tools/crowbar.rsi
    color: "#FF0000"
    layers:
    - state: icon
      map: [ "enum.Layers.Base" ]
    - state: 007
  - type: Fixtures
    fixtures:
      fix1:
        shape: !type:PhysShapeAabb
          bounds: "-0.25,-0.25,0.25,0.25"
        mask: [ItemMask]
        density: 20.5
      "fix 2":
        shape: !type:PhysShapeCircle {}
        hard: false
        mode: None
  - type: Empty
    list: []
    map: {}
    single: !type:Wrap [1]
    pair: !type:Pair [1, 2]
    scalar: !type:Scaled 2.5
"##;

    #[test]
    fn ron_roundtrip_preserves_tree() {
        let tree = parse_yaml(SOURCE).unwrap();
        let ron = to_ron_string(&tree);
        let back = parse_ron(&ron).unwrap_or_else(|e| panic!("{e}\n{ron}"));
        assert_eq!(normalize(&back), normalize(&tree), "{ron}");
        assert!(ron.contains("PhysShapeAabb("), "{ron}");
        assert!(ron.contains("mode: \"None\""), "{ron}");
    }

    #[test]
    fn yaml_roundtrip_preserves_tree() {
        let tree = parse_yaml(SOURCE).unwrap();
        let yaml = to_yaml_string(&tree);
        let back = parse_yaml(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
        assert_eq!(normalize(&back), normalize(&tree), "{yaml}");
        // И RON обратно в YAML.
        let from_ron = parse_ron(&to_ron_string(&tree)).unwrap();
        let again = parse_yaml(&to_yaml_string(&from_ron)).unwrap();
        assert_eq!(normalize(&again), normalize(&tree));
    }

    /// Сравнение без учёта кавычек и вида пустого значения.
    fn normalize(node: &DataNode) -> DataNode {
        let mut copy = node.clone();
        match &mut copy.value {
            Value::Scalar(scalar) => {
                if node.is_null() {
                    scalar.text = "null".into();
                }
                scalar.quoted = false;
            }
            Value::Seq(items) => items.iter_mut().for_each(|n| *n = normalize(n)),
            Value::Map(map) => map.0.iter_mut().for_each(|(_, n)| *n = normalize(n)),
        }
        copy
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Shape {
        Circle { radius: f32 },
        Scaled(f32),
        Pair(i32, i32),
        Point,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Body {
        name: String,
        shapes: Vec<Shape>,
        mass: Option<f64>,
        offset: (f32, f32),
        extra: DataNode,
    }

    #[test]
    fn typed_value_roundtrips_through_both_formats() {
        let body = Body {
            name: "true".into(),
            shapes: vec![
                Shape::Circle { radius: 0.35 },
                Shape::Scaled(2.0),
                Shape::Pair(1, -2),
                Shape::Point,
            ],
            mass: None,
            offset: (0.1, -3.0),
            extra: parse_ron("Thing(a: [1, 2], b: \"x y\")").unwrap(),
        };
        let tree = to_node(&body).unwrap();
        for text in [to_ron_string(&tree), to_yaml_string(&tree)] {
            let parsed = if text.trim_start().starts_with('(') {
                parse_ron(&text)
            } else {
                parse_yaml(&text)
            }
            .unwrap_or_else(|e| panic!("{e}\n{text}"));
            let back: Body = from_node(&parsed).unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert_eq!(back, body, "{text}");
        }
    }
}
