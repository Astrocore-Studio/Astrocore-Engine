//! Разбор RON в дерево данных.
//!
//! RON — формат собственного контента Astrocore. Структура та же, что у YAML SS14:
//!
//! | RON | Узел | YAML |
//! |---|---|---|
//! | `(a: 1)` | отображение | `{a: 1}` |
//! | `{"k": 1}` | отображение | `{k: 1}` |
//! | `[1, 2]`, `(1, 2)` | последовательность | `[1, 2]` |
//! | `Foo(a: 1)` | отображение с тегом `type:Foo` | `!type:Foo {a: 1}` |
//! | `Foo(x)` | значение `x` с тегом `type:Foo` | `!type:Foo x` |
//! | `Foo` | скаляр без кавычек | `Foo` |
//! | `Some(x)`, `None` | `x`, пустое значение | `x`, `null` |
//! | `"text"`, `'c'`, `r#"raw"#` | строка | `"text"` |

use crate::node::{DataError, DataMap, DataNode, Scalar, TYPE_TAG_PREFIX, Value};

const MAX_DEPTH: usize = 128;

/// Разбирает документ RON.
pub fn parse_ron(text: &str) -> Result<DataNode, DataError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parser = RonParser {
        src: text.as_bytes(),
        text,
        pos: 0,
        line: 1,
        depth: 0,
    };
    parser.skip_trivia()?;
    // Расширения RON (`#![enable(...)]`) структуру дерева не меняют.
    while parser.src[parser.pos..].starts_with(b"#!") {
        while parser.pos < parser.src.len() && parser.src[parser.pos] != b'\n' {
            parser.pos += 1;
        }
        parser.skip_trivia()?;
    }
    let node = parser.value()?;
    parser.skip_trivia()?;
    if parser.pos < parser.src.len() {
        return Err(parser.error("лишний текст после значения"));
    }
    Ok(node)
}

struct RonParser<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    line: u32,
    depth: usize,
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_part(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

impl RonParser<'_> {
    fn error(&self, message: &str) -> DataError {
        DataError::new(format!("RON: {message}"), self.line)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        if byte == b'\n' {
            self.line += 1;
        }
        Some(byte)
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), DataError> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(self.error(&format!("ожидался символ `{}`", byte as char)))
        }
    }

    fn skip_trivia(&mut self) -> Result<(), DataError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.bump();
                }
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'/') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.bump();
                    }
                }
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    let mut depth = 1;
                    while depth > 0 {
                        match (self.bump(), self.peek()) {
                            (Some(b'*'), Some(b'/')) => {
                                self.bump();
                                depth -= 1;
                            }
                            (Some(b'/'), Some(b'*')) => {
                                self.bump();
                                depth += 1;
                            }
                            (None, _) => return Err(self.error("незакрытый комментарий")),
                            _ => {}
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self) -> Result<DataNode, DataError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("слишком глубокая вложенность"));
        }
        self.skip_trivia()?;
        let line = self.line;
        let node = match self.peek() {
            None => return Err(self.error("ожидалось значение")),
            Some(b'[') => {
                self.bump();
                DataNode::seq(self.sequence(b']')?)
            }
            Some(b'{') => {
                self.bump();
                DataNode::map(self.braced_map()?)
            }
            Some(b'(') => {
                self.bump();
                self.parenthesized()?
            }
            Some(b'"') => DataNode::string(self.string()?),
            Some(b'\'') => DataNode::string(self.char_literal()?),
            Some(b'r') if matches!(self.src.get(self.pos + 1), Some(b'"' | b'#')) => {
                DataNode::string(self.raw_string()?)
            }
            Some(byte) if is_ident_start(byte) => self.identifier_value()?,
            Some(byte) if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.') => {
                DataNode::plain(self.number()?)
            }
            Some(_) => return Err(self.error("ожидалось значение")),
        };
        self.depth -= 1;
        Ok(if node.line == 0 {
            node.with_line(line)
        } else {
            node
        })
    }

    fn identifier(&mut self) -> &str {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_part) {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn identifier_value(&mut self) -> Result<DataNode, DataError> {
        let name = self.identifier().to_owned();
        // Числа с плавающей точкой `inf` и `NaN` в RON пишутся словами.
        match name.as_str() {
            "true" | "false" | "inf" | "NaN" => return Ok(DataNode::plain(name)),
            "None" => return Ok(DataNode::null()),
            _ => {}
        }
        self.skip_trivia()?;
        if !self.eat(b'(') {
            return Ok(DataNode::plain(name));
        }
        let inner = self.parenthesized()?;
        if name == "Some" {
            return Ok(inner);
        }
        Ok(inner.with_tag(format!("{TYPE_TAG_PREFIX}{name}")))
    }

    /// Содержимое круглых скобок: поля структуры или элементы кортежа.
    /// Один позиционный элемент — само значение (как у newtype в RON).
    fn parenthesized(&mut self) -> Result<DataNode, DataError> {
        self.skip_trivia()?;
        if self.eat(b')') {
            return Ok(DataNode::map(DataMap::new()));
        }
        if self.at_field() {
            let mut map = DataMap::new();
            loop {
                self.skip_trivia()?;
                if self.eat(b')') {
                    break;
                }
                if self.src[self.pos..].starts_with(b"r#") {
                    self.pos += 2;
                }
                if !self.peek().is_some_and(is_ident_start) {
                    return Err(self.error("ожидалось имя поля"));
                }
                let key = self.identifier().to_owned();
                self.skip_trivia()?;
                self.expect(b':')?;
                let value = self.value()?;
                if map.insert(key.clone(), value).is_some() {
                    return Err(self.error(&format!("поле `{key}` задано дважды")));
                }
                self.skip_trivia()?;
                if !self.eat(b',') {
                    self.skip_trivia()?;
                    self.expect(b')')?;
                    break;
                }
            }
            return Ok(DataNode::map(map));
        }
        let mut items = self.sequence(b')')?;
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            DataNode::seq(items)
        })
    }

    /// Впереди `имя:` — значит, это поля структуры.
    fn at_field(&self) -> bool {
        let mut pos = self.pos;
        if self.src[pos..].starts_with(b"r#") {
            pos += 2;
        }
        if !self.src.get(pos).copied().is_some_and(is_ident_start) {
            return false;
        }
        while self.src.get(pos).copied().is_some_and(is_ident_part) {
            pos += 1;
        }
        while matches!(self.src.get(pos), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            pos += 1;
        }
        self.src.get(pos) == Some(&b':')
    }

    fn sequence(&mut self, close: u8) -> Result<Vec<DataNode>, DataError> {
        let mut items = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.eat(close) {
                return Ok(items);
            }
            items.push(self.value()?);
            self.skip_trivia()?;
            if !self.eat(b',') {
                self.skip_trivia()?;
                self.expect(close)?;
                return Ok(items);
            }
        }
    }

    fn braced_map(&mut self) -> Result<DataMap, DataError> {
        let mut map = DataMap::new();
        loop {
            self.skip_trivia()?;
            if self.eat(b'}') {
                return Ok(map);
            }
            let key = self.value()?;
            let Value::Scalar(Scalar { text: key, .. }) = key.value else {
                return Err(self.error("ключ отображения должен быть скаляром"));
            };
            self.skip_trivia()?;
            self.expect(b':')?;
            let value = self.value()?;
            if map.insert(key.clone(), value).is_some() {
                return Err(self.error(&format!("ключ `{key}` задан дважды")));
            }
            self.skip_trivia()?;
            if !self.eat(b',') {
                self.skip_trivia()?;
                self.expect(b'}')?;
                return Ok(map);
            }
        }
    }

    fn number(&mut self) -> Result<String, DataError> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        if self.src[self.pos..].starts_with(b"inf") || self.src[self.pos..].starts_with(b"NaN") {
            self.pos += 3;
            return Ok(self.text[start..self.pos].to_owned());
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
        {
            // Знак показателя степени: `1e-3`.
            if matches!(self.peek(), Some(b'e' | b'E'))
                && matches!(self.src.get(self.pos + 1), Some(b'-' | b'+'))
                && !self.text[start..self.pos].contains("0x")
            {
                self.pos += 1;
            }
            self.pos += 1;
        }
        let text = self.text[start..self.pos].replace('_', "");
        if text.is_empty() || text == "-" || text == "+" || text == "." {
            return Err(self.error("ожидалось число"));
        }
        Ok(text)
    }

    fn escape(&mut self) -> Result<char, DataError> {
        match self.bump() {
            Some(b'n') => Ok('\n'),
            Some(b'r') => Ok('\r'),
            Some(b't') => Ok('\t'),
            Some(b'0') => Ok('\0'),
            Some(b'\\') => Ok('\\'),
            Some(b'"') => Ok('"'),
            Some(b'\'') => Ok('\''),
            Some(b'x') => {
                let hex = self
                    .text
                    .get(self.pos..self.pos + 2)
                    .ok_or_else(|| self.error("неполная последовательность \\x"))?;
                self.pos += 2;
                u8::from_str_radix(hex, 16)
                    .ok()
                    .filter(u8::is_ascii)
                    .map(char::from)
                    .ok_or_else(|| self.error("неверная последовательность \\x"))
            }
            Some(b'u') => {
                self.expect(b'{')?;
                let start = self.pos;
                while self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                    self.pos += 1;
                }
                let code = u32::from_str_radix(&self.text[start..self.pos], 16).ok();
                self.expect(b'}')?;
                code.and_then(char::from_u32)
                    .ok_or_else(|| self.error("неверная последовательность \\u"))
            }
            _ => Err(self.error("неизвестная управляющая последовательность")),
        }
    }

    fn string(&mut self) -> Result<String, DataError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while !matches!(self.peek(), None | Some(b'"' | b'\\')) {
                self.bump();
            }
            out.push_str(&self.text[start..self.pos]);
            match self.bump() {
                Some(b'"') => return Ok(out),
                Some(b'\\') => {
                    // Обратная косая черта перед переводом строки склеивает строки.
                    if self.eat(b'\n') {
                        while matches!(self.peek(), Some(b' ' | b'\t')) {
                            self.bump();
                        }
                    } else {
                        out.push(self.escape()?);
                    }
                }
                _ => return Err(self.error("незакрытая строка")),
            }
        }
    }

    fn char_literal(&mut self) -> Result<String, DataError> {
        self.expect(b'\'')?;
        let ch = if self.eat(b'\\') {
            self.escape()?
        } else {
            let ch = self.text[self.pos..]
                .chars()
                .next()
                .ok_or_else(|| self.error("незакрытый символ"))?;
            self.pos += ch.len_utf8();
            ch
        };
        self.expect(b'\'')?;
        Ok(ch.to_string())
    }

    fn raw_string(&mut self) -> Result<String, DataError> {
        self.expect(b'r')?;
        let mut hashes = 0;
        while self.eat(b'#') {
            hashes += 1;
        }
        self.expect(b'"')?;
        let start = self.pos;
        loop {
            match self.bump() {
                None => return Err(self.error("незакрытая строка")),
                Some(b'"') => {
                    let end = self.pos - 1;
                    if self.src[self.pos..].len() >= hashes
                        && self.src[self.pos..self.pos + hashes]
                            .iter()
                            .all(|&b| b == b'#')
                    {
                        self.pos += hashes;
                        return Ok(self.text[start..end].to_owned());
                    }
                }
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml::parse_yaml;

    #[test]
    fn ron_and_yaml_give_the_same_tree() {
        let ron = parse_ron(
            r#"// прототипы
            [
                entity(
                    id: "Crowbar",
                    parent: ["BaseItem", "BaseTool"],
                    components: [
                        Sprite(sprite: "Objects/Tools/crowbar.rsi", state: "icon"),
                        Item(size: Small),
                        Fixtures(fixtures: {"fix1": (shape: PhysShapeCircle(radius: 0.35), density: 20)}),
                    ],
                ),
            ]"#,
        )
        .unwrap();
        let yaml = parse_yaml(
            r#"
- !type:entity
  id: "Crowbar"
  parent: ["BaseItem", "BaseTool"]
  components:
  - !type:Sprite
    sprite: "Objects/Tools/crowbar.rsi"
    state: "icon"
  - !type:Item
    size: Small
  - !type:Fixtures
    fixtures:
      "fix1":
        shape: !type:PhysShapeCircle
          radius: 0.35
        density: 20
"#,
        )
        .unwrap();
        assert_eq!(ron, yaml);
        assert_eq!(ron.as_seq().unwrap()[0].type_name(), Some("entity"));
    }

    #[test]
    fn options_tuples_and_newtypes() {
        let node =
            parse_ron("(a: Some(1), b: None, c: (1, 2), d: Wrap(3), e: Unit, f: ())").unwrap();
        assert_eq!(node.get("a").unwrap().as_str(), Some("1"));
        assert!(node.get("b").unwrap().is_null());
        assert_eq!(node.get("c").unwrap().as_seq().unwrap().len(), 2);
        let wrapped = node.get("d").unwrap();
        assert_eq!(
            (wrapped.type_tag(), wrapped.as_str()),
            (Some("Wrap"), Some("3"))
        );
        assert!(!node.get("e").unwrap().as_scalar().unwrap().quoted);
        assert!(node.get("f").unwrap().as_map().unwrap().is_empty());
    }

    #[test]
    fn strings_numbers_and_comments() {
        let node = parse_ron(
            "#![enable(implicit_some)]\n/* a /* nested */ */ [\"a\\n\\u{41}\", r#\"x\"y\"#, 'z', -1.5e-3, 0x1F, 1_000, true]",
        )
        .unwrap();
        let texts: Vec<_> = node
            .as_seq()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(
            texts,
            ["a\nA", "x\"y", "z", "-1.5e-3", "0x1F", "1000", "true"]
        );
    }

    #[test]
    fn errors_carry_line() {
        let error = parse_ron("(\n  a: 1,\n  a: 2,\n)").unwrap_err();
        assert_eq!(error.line, 3);
        assert!(parse_ron("[1, 2").is_err());
        assert!(parse_ron("(a: 1) junk").is_err());
    }
}
