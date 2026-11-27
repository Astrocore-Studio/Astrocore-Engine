//! Совместимость файлов `.ftl` SS14 с fluent-rs (ADR-0018).
//!
//! Robust читает переводы через Linguini, а он мягче спецификации Fluent в двух местах,
//! которыми пользуются файлы SS14 @ 2f4a2aad (`Robust.Shared/Localization/LocalizationManager.cs`
//! @ 1afd692):
//!
//! 1. **Переменная в именованном аргументе термина**: `{ -term(name: $var) }`. По спецификации
//!    там допустимы только литералы, и fluent-rs отбрасывает всё сообщение. Такой вызов
//!    заменяется ссылкой на служебное сообщение — копию тела термина, где `$name` заменено
//!    значением аргумента (`$var` или литералом). Ссылка на сообщение видит аргументы
//!    вызывающего сообщения, поэтому `$var` доходит до текста.
//! 2. **Метка порядка байтов** в начале файла: fluent-rs не разбирает первую запись.
//!
//! Отличие от Linguini: переменная тела термина, которую вызов не передал, у Linguini
//! пуста, а здесь берётся из аргументов вызывающего сообщения.

use std::collections::HashMap;

/// Префикс служебных сообщений, в которые разворачиваются вызовы терминов.
pub(crate) const GENERATED_PREFIX: &str = "astrocore-term-";

/// Текст без метки порядка байтов.
pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{FEFF}').unwrap_or(text)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Длина идентификатора в начале строки (в байтах; идентификаторы — ASCII).
fn ident_len(text: &str) -> usize {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if is_ident_start(c) => {}
        _ => return 0,
    }
    1 + chars.take_while(|c| is_ident_char(*c)).count()
}

/// Строка начинает новую запись: сообщение, термин или комментарий с первой колонки.
/// Продолжение записи — строки с отступом, пустые и строки с `}`, `[`, `*`, `.`.
fn starts_entry(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !matches!(c, '}' | '[' | '*' | '.'))
}

/// Тела терминов файла: идентификатор без `-` → текст после `=` до следующей записи.
pub(crate) fn collect_terms(text: &str, terms: &mut HashMap<String, String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        let Some(rest) = line.strip_prefix('-') else {
            continue;
        };
        let len = ident_len(rest);
        if len == 0 {
            continue;
        }
        let (id, after) = rest.split_at(len);
        let Some(value) = after.trim_start().strip_prefix('=') else {
            continue;
        };
        let mut body = value.to_owned();
        while index < lines.len() && !starts_entry(lines[index]) {
            body.push('\n');
            body.push_str(lines[index]);
            index += 1;
        }
        terms.insert(id.to_owned(), body.trim_end().to_owned());
    }
}

/// Аргументы вызова `name: value, ...` до закрывающей скобки; `None` — не похоже на вызов.
/// Возвращает пары и длину до `)` включительно.
fn parse_arguments(text: &str) -> Option<(Vec<(String, String)>, usize)> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for (offset, c) in text.char_indices() {
        if quoted {
            current.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                current.push(c);
            }
            ',' | ')' => {
                if !current.trim().is_empty() {
                    let (name, value) = current.split_once(':')?;
                    let name = name.trim();
                    if ident_len(name) != name.len() || name.is_empty() {
                        return None;
                    }
                    arguments.push((name.to_owned(), value.trim().to_owned()));
                }
                current.clear();
                if c == ')' {
                    return Some((arguments, offset + 1));
                }
            }
            '{' | '}' | '\n' => return None,
            _ => current.push(c),
        }
    }
    None
}

/// Тело термина с подставленными аргументами вместо `$name`.
fn substitute(body: &str, arguments: &[(String, String)]) -> String {
    let mut result = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        result.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let len = ident_len(after);
        let name = &after[..len];
        match arguments
            .iter()
            .find(|(n, _)| n == name)
            .filter(|_| len > 0)
        {
            Some((_, value)) => result.push_str(value),
            None => {
                result.push('$');
                result.push_str(name);
            }
        }
        rest = &after[len..];
    }
    result.push_str(rest);
    result
}

/// Вызовы терминов с переменными в аргументах заменяются ссылками на служебные сообщения,
/// которые дописываются в конец файла. Возвращает текст и число дописанных сообщений.
pub(crate) fn rewrite_term_calls(
    text: &str,
    terms: &HashMap<String, String>,
    counter: &mut usize,
) -> (String, usize) {
    let mut output = String::with_capacity(text.len());
    let mut generated = String::new();
    let mut added = 0;
    let mut rest = text;
    while let Some(at) = rest.find('-') {
        let (before, from) = rest.split_at(at);
        output.push_str(before);
        // Вызов термина стоит сразу после `{` (пробелы допустимы).
        let opens_placeable = output.trim_end().ends_with('{');
        let len = ident_len(&from[1..]);
        let call = (opens_placeable && len > 0)
            .then(|| {
                let id = &from[1..1 + len];
                let after = &from[1 + len..];
                let (arguments, used) = parse_arguments(after.strip_prefix('(')?)?;
                if !arguments.iter().any(|(_, v)| v.starts_with('$')) {
                    return None;
                }
                let body = terms.get(id)?;
                Some((id, arguments, 1 + len + 1 + used, body))
            })
            .flatten();
        let Some((id, arguments, used, body)) = call else {
            output.push('-');
            rest = &from[1..];
            continue;
        };
        *counter += 1;
        added += 1;
        let message = format!("{GENERATED_PREFIX}{counter}-{id}");
        generated.push_str(&format!("\n{message} ={}\n", substitute(body, &arguments)));
        output.push_str(&message);
        rest = &from[used..];
    }
    output.push_str(rest);
    if !generated.is_empty() {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&generated);
    }
    (output, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "-edible-satiated = { $satiated ->\n    [true] {\" \"}You can't { $verb } more.\n  *[false] {\"\"}\n}\n\n# comment\nnom = Nom.{ -edible-satiated(satiated: $satiated, verb: \"eat\") }\nstate = {$address} : {-state-name(state:$state)}\nplain = { -edible-satiated(satiated: \"true\", verb: \"eat\") }\nhyphen = well-known - text\n";

    #[test]
    fn terms_are_collected_to_next_entry() {
        let mut terms = HashMap::new();
        collect_terms(
            &format!(
                "{SOURCE}-state-name = {{ $state ->\n    [normal] Normal\n   *[invalid] Invalid\n}}\n"
            ),
            &mut terms,
        );
        assert_eq!(
            terms["edible-satiated"],
            " { $satiated ->\n    [true] {\" \"}You can't { $verb } more.\n  *[false] {\"\"}\n}"
        );
        assert!(terms["state-name"].ends_with("Invalid\n}"));
    }

    #[test]
    fn calls_with_variables_become_message_references() {
        let mut terms = HashMap::new();
        collect_terms(SOURCE, &mut terms);
        terms.insert("state-name".into(), " { $state -> *[x] X }".into());
        let mut counter = 0;
        let (text, added) = rewrite_term_calls(SOURCE, &terms, &mut counter);
        assert_eq!(added, 2);
        assert!(text.contains("nom = Nom.{ astrocore-term-1-edible-satiated }"));
        assert!(text.contains("state = {$address} : {astrocore-term-2-state-name}"));
        // Вызов с одними литералами допустим и не меняется; дефисы в тексте — тоже.
        assert!(text.contains("plain = { -edible-satiated(satiated: \"true\", verb: \"eat\") }"));
        assert!(text.contains("hyphen = well-known - text"));
        assert!(text.contains(
            "astrocore-term-1-edible-satiated = { $satiated ->\n    [true] {\" \"}You can't { \"eat\" } more."
        ));
        assert!(text.contains("astrocore-term-2-state-name = { $state -> *[x] X }"));
        assert_eq!(counter, 2);
    }

    #[test]
    fn unknown_terms_and_bom_are_left_or_stripped() {
        let (text, added) =
            rewrite_term_calls("a = { -missing(x: $x) }\n", &HashMap::new(), &mut 0);
        assert_eq!((text.as_str(), added), ("a = { -missing(x: $x) }\n", 0));
        assert_eq!(strip_bom("\u{FEFF}a = b"), "a = b");
        assert_eq!(
            substitute("{ $a } { $ab } { $b }", &[("a".into(), "$z".into())]),
            "{ $z } { $ab } { $b }"
        );
    }
}
