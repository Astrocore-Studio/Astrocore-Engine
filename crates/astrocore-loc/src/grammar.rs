//! Грамматика сообщений: сущность как аргумент перевода и функции, берущие из неё род и
//! признак имени собственного. Образец — `LocalizationManager.Functions.cs`
//! (`GENDER`, `SUBJECT`, `OBJECT`, `DAT-OBJ`, `GENITIVE`, `POSS-ADJ`, `POSS-PRONOUN`,
//! `REFLEXIVE`, `COUNTER`, `CONJUGATE-BE`, `CONJUGATE-HAVE`, `CONJUGATE-BASIC`, `PROPER`,
//! `THE`, `ATTRIB`, `INDEFINITE`, `ESCAPE`, `ESCAPE-PARAM`), `LocalizationManager.Entity.cs`
//! (`TryGetEntityLocAttrib`) и `GrammarComponent` Robust @ 1afd692.
//!
//! Местоимения и артикли — служебные сообщения `zzzz-*` из `_engine_lib.ftl` языка (своя
//! копия en-US лежит в крейте). Функции переводят их отдельным набором сообщений: функции
//! fluent-rs не видят свой набор.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use bevy_ecs::component::Component;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::types::FluentType;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use serde::{Deserialize, Serialize};
use unic_langid::LanguageIdentifier;

/// Служебные сообщения en-US.
pub(crate) const ENGINE_LIB_EN_US: &str = include_str!("../locale/en-US/_engine_lib.ftl");

/// Сущность как аргумент сообщения: название и грамматические признаки (`gender`,
/// `proper`, `indefinite`, `counter` и любые другие для `ATTRIB`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityArg {
    pub name: String,
    pub attributes: BTreeMap<String, String>,
}

impl EntityArg {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: BTreeMap::new(),
        }
    }

    /// С признаком.
    pub fn with(mut self, attribute: &str, value: impl Into<String>) -> Self {
        self.attributes.insert(attribute.to_owned(), value.into());
        self
    }

    /// Род: `male`, `female`, `epicene`, `neuter` (по умолчанию).
    pub fn gender(&self) -> String {
        self.attributes
            .get("gender")
            .map(|g| g.to_lowercase())
            .unwrap_or_else(|| "neuter".into())
    }

    /// Имя собственное (`proper`).
    pub fn proper(&self) -> bool {
        self.attributes
            .get("proper")
            .is_some_and(|p| p.eq_ignore_ascii_case("true"))
    }
}

impl FluentType for EntityArg {
    fn duplicate(&self) -> Box<dyn FluentType + Send> {
        Box::new(self.clone())
    }

    fn as_string(&self, _: &intl_memoizer::IntlLangMemoizer) -> Cow<'static, str> {
        Cow::Owned(self.name.clone())
    }

    fn as_string_threadsafe(
        &self,
        _: &intl_memoizer::concurrent::IntlLangMemoizer,
    ) -> Cow<'static, str> {
        Cow::Owned(self.name.clone())
    }
}

/// Грамматические признаки сущности (`GrammarComponent`): `gender`, `proper` и другие.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grammar {
    pub attributes: BTreeMap<String, String>,
}

impl Grammar {
    pub fn gender(&self) -> Option<&str> {
        self.attributes.get("gender").map(String::as_str)
    }

    pub fn set_gender(&mut self, gender: Option<&str>) {
        match gender {
            Some(g) => self.attributes.insert("gender".into(), g.to_lowercase()),
            None => self.attributes.remove("gender"),
        };
    }

    pub fn proper(&self) -> Option<bool> {
        self.attributes
            .get("proper")
            .map(|p| p.eq_ignore_ascii_case("true"))
    }

    pub fn set_proper(&mut self, proper: Option<bool>) {
        match proper {
            Some(p) => self.attributes.insert("proper".into(), p.to_string()),
            None => self.attributes.remove("proper"),
        };
    }
}

fn entity_of<'a>(value: &'a FluentValue<'_>) -> Option<&'a EntityArg> {
    match value {
        FluentValue::Custom(custom) => custom.as_any().downcast_ref::<EntityArg>(),
        _ => None,
    }
}

fn text_of(value: &FluentValue<'_>) -> String {
    match value {
        FluentValue::String(s) => s.to_string(),
        FluentValue::Number(n) => n.as_string().to_string(),
        FluentValue::Custom(_) => entity_of(value).map(|e| e.name.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn string(text: impl Into<String>) -> FluentValue<'static> {
    FluentValue::from(text.into())
}

/// `GENDER($ent)`.
fn gender(args: &[FluentValue<'_>]) -> FluentValue<'static> {
    string(
        args.first()
            .and_then(entity_of)
            .map_or_else(|| "neuter".into(), EntityArg::gender),
    )
}

/// `PROPER($ent)`.
fn proper(args: &[FluentValue<'_>]) -> FluentValue<'static> {
    string(
        args.first()
            .and_then(entity_of)
            .is_some_and(EntityArg::proper)
            .to_string(),
    )
}

/// `ATTRIB($ent, "имя")`: признак или `other`.
fn attrib(args: &[FluentValue<'_>]) -> FluentValue<'static> {
    let value = args
        .first()
        .and_then(entity_of)
        .zip(args.get(1).map(text_of))
        .and_then(|(entity, name)| entity.attributes.get(&name).cloned());
    string(value.unwrap_or_else(|| "other".into()))
}

/// Экранирование разметки (`FormattedMessage.EscapeText`).
pub fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('[', "\\[")
}

/// Экранирование параметра тега (`FormattedMessage.EscapeStringParameter`).
pub fn escape_parameter(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

// INDEFINITE.

const INDEF_EXCEPTIONS: [&str; 3] = ["euler", "heir", "honest"];
const INDEF_CHARS: &str = "aedhilmnorsx";

fn upper(c: char) -> bool {
    c.is_ascii_uppercase()
}

/// `(?!…)[FHLMNRSX][A-Z]` из Robust: аббревиатуры, читаемые с гласной («an FBI»).
fn fjo_match(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let vowel = |c: Option<char>| c.is_some_and(|c| "AEIOU".contains(c));
    let blocked = |i: usize| {
        let s: String = chars[i..].iter().take(4).collect();
        if s.starts_with("FJO") || s.starts_with("SQU") {
            return true;
        }
        if at(i).is_some_and(|c| "HLMNS".contains(c))
            && at(i + 1) == Some('Y')
            && at(i + 2).is_some()
        {
            return true;
        }
        if at(i) == Some('R') && at(i + 1) == Some('Y') && matches!(at(i + 2), Some('E' | 'O')) {
            return true;
        }
        // (F[LR]?|[HL]|MN?|N|RH?|S[CHKLMNPTVW]?|X(YL)?)[AEIOU]
        let heads: Vec<usize> = match at(i) {
            Some('F') => {
                let mut v = vec![1];
                if matches!(at(i + 1), Some('L' | 'R')) {
                    v.push(2);
                }
                v
            }
            Some('H' | 'L' | 'N') => vec![1],
            Some('M') => {
                let mut v = vec![1];
                if at(i + 1) == Some('N') {
                    v.push(2);
                }
                v
            }
            Some('R') => {
                let mut v = vec![1];
                if at(i + 1) == Some('H') {
                    v.push(2);
                }
                v
            }
            Some('S') => {
                let mut v = vec![1];
                if at(i + 1).is_some_and(|c| "CHKLMNPTVW".contains(c)) {
                    v.push(2);
                }
                v
            }
            Some('X') => {
                let mut v = vec![1];
                if at(i + 1) == Some('Y') && at(i + 2) == Some('L') {
                    v.push(3);
                }
                v
            }
            _ => Vec::new(),
        };
        heads.into_iter().any(|len| vowel(at(i + len)))
    };
    (0..chars.len()).any(|i| {
        at(i).is_some_and(|c| "FHLMNRSX".contains(c)) && at(i + 1).is_some_and(upper) && !blocked(i)
    })
}

fn starts_with_any(word: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| word.starts_with(p))
}

/// Артикль `a` или `an` по слову (`FuncIndefinite`).
pub fn indefinite_article(input: &str) -> &'static str {
    let Some(word) = input
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|w| !w.is_empty())
    else {
        return "an";
    };
    let lower = word.to_lowercase();
    if INDEF_EXCEPTIONS.iter().any(|e| lower.starts_with(e)) {
        return "an";
    }
    if lower.starts_with("hour") && !lower.starts_with("houri") {
        return "an";
    }
    let first_in = |set: &str| lower.chars().next().is_some_and(|c| set.contains(c));
    if lower.chars().count() == 1 {
        return if first_in(INDEF_CHARS) { "an" } else { "a" };
    }
    if fjo_match(word) {
        return "an";
    }
    let mut chars = lower.chars();
    let (c0, c1, c2) = (chars.next(), chars.next(), chars.next());
    // ^e[uw], ^onc?e\b, ^uni([^nmd]|mo), ^u[bcfhjkqrst][aeiou]
    if c0 == Some('e') && matches!(c1, Some('u' | 'w')) {
        return "a";
    }
    let word_end = |rest: &str| {
        rest.chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    };
    if let Some(rest) = lower
        .strip_prefix("once")
        .or_else(|| lower.strip_prefix("one"))
        && word_end(rest)
    {
        return "a";
    }
    if let Some(rest) = lower.strip_prefix("uni") {
        let mut r = rest.chars();
        match r.next() {
            Some(c) if !"nmd".contains(c) => return "a",
            Some('m') if r.next() == Some('o') => return "a",
            _ => {}
        }
    }
    if c0 == Some('u')
        && c1.is_some_and(|c| "bcfhjkqrst".contains(c))
        && c2.is_some_and(|c| "aeiou".contains(c))
    {
        return "a";
    }
    // ^U[NK][AIEO] по исходному написанию.
    let mut original = word.chars();
    if original.next() == Some('U')
        && matches!(original.next(), Some('N' | 'K'))
        && matches!(original.next(), Some('A' | 'I' | 'E' | 'O'))
    {
        return "a";
    }
    if word == word.to_uppercase() {
        return if first_in(INDEF_CHARS) { "an" } else { "a" };
    }
    if first_in("aeiou") {
        return "an";
    }
    // ^y(b[lor]|cl[ea]|fere|gg|p[ios]|rou|tt)
    if let Some(rest) = lower.strip_prefix('y') {
        let mut r = rest.chars();
        let (a, b) = (r.next(), r.next());
        let y = matches!((a, b), (Some('b'), Some('l' | 'o' | 'r')))
            || (a == Some('c') && b == Some('l') && matches!(r.next(), Some('e' | 'a')))
            || starts_with_any(rest, &["fere", "gg", "rou", "tt"])
            || matches!((a, b), (Some('p'), Some('i' | 'o' | 's')));
        if y {
            return "an";
        }
    }
    "a"
}

/// `INDEFINITE($x)`: у сущности — признак `indefinite`, иначе по названию.
fn indefinite(args: &[FluentValue<'_>]) -> FluentValue<'static> {
    let Some(value) = args.first() else {
        return string("an");
    };
    if let Some(entity) = entity_of(value) {
        if let Some(indef) = entity.attributes.get("indefinite") {
            return string(indef.clone());
        }
        if entity.name.is_empty() {
            return string("");
        }
        return string(indefinite_article(&entity.name));
    }
    let text = text_of(value);
    if text.is_empty() {
        return string("");
    }
    string(indefinite_article(&text))
}

/// Набор служебных сообщений языка с функциями `GENDER` и `PROPER`.
pub(crate) fn grammar_bundle(
    language: &LanguageIdentifier,
    extra: Option<String>,
) -> Arc<FluentBundle<FluentResource>> {
    let mut bundle = FluentBundle::new_concurrent(vec![language.clone()]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_function("GENDER", |args, _| gender(args));
    let _ = bundle.add_function("PROPER", |args, _| proper(args));
    if let Ok(resource) = FluentResource::try_new(ENGINE_LIB_EN_US.to_owned()) {
        let _ = bundle.add_resource(resource);
    }
    if let Some(text) = extra
        && let Ok(resource) = FluentResource::try_new(text)
    {
        bundle.add_resource_overriding(resource);
    }
    Arc::new(bundle)
}

fn format(
    grammar: &FluentBundle<FluentResource>,
    id: &str,
    args: &FluentArgs<'_>,
) -> FluentValue<'static> {
    let Some(pattern) = grammar.get_message(id).and_then(|m| m.value()) else {
        return string(id);
    };
    let mut errors = Vec::new();
    string(
        grammar
            .format_pattern(pattern, Some(args), &mut errors)
            .into_owned(),
    )
}

fn ent_args<'a>(value: Option<&FluentValue<'a>>) -> FluentArgs<'static> {
    let mut args = FluentArgs::new();
    if let Some(value) = value {
        args.set(
            "ent",
            match value {
                FluentValue::Custom(custom) => FluentValue::Custom(custom.duplicate()),
                other => FluentValue::from(text_of(other)),
            },
        );
    }
    args
}

/// Ставит грамматические функции в набор сообщений языка.
pub(crate) fn install_grammar(
    bundle: &mut FluentBundle<FluentResource>,
    grammar: &Arc<FluentBundle<FluentResource>>,
) {
    let _ = bundle.add_function("GENDER", |args, _| gender(args));
    let _ = bundle.add_function("PROPER", |args, _| proper(args));
    let _ = bundle.add_function("ATTRIB", |args, _| attrib(args));
    let _ = bundle.add_function("INDEFINITE", |args, _| indefinite(args));
    let _ = bundle.add_function("ESCAPE", |args, _| {
        string(escape_text(&args.first().map(text_of).unwrap_or_default()))
    });
    let _ = bundle.add_function("ESCAPE-PARAM", |args, _| {
        string(escape_parameter(
            &args.first().map(text_of).unwrap_or_default(),
        ))
    });
    for (name, message) in [
        ("THE", "zzzz-the"),
        ("SUBJECT", "zzzz-subject-pronoun"),
        ("OBJECT", "zzzz-object-pronoun"),
        ("DAT-OBJ", "zzzz-dat-object"),
        ("GENITIVE", "zzzz-genitive"),
        ("POSS-ADJ", "zzzz-possessive-adjective"),
        ("POSS-PRONOUN", "zzzz-possessive-pronoun"),
        ("REFLEXIVE", "zzzz-reflexive-pronoun"),
        ("CONJUGATE-BE", "zzzz-conjugate-be"),
        ("CONJUGATE-HAVE", "zzzz-conjugate-have"),
    ] {
        let grammar = Arc::clone(grammar);
        let _ = bundle.add_function(name, move |args, _| {
            format(&grammar, message, &ent_args(args.first()))
        });
    }
    let grammar_basic = Arc::clone(grammar);
    let _ = bundle.add_function("CONJUGATE-BASIC", move |args, _| {
        let mut values = ent_args(args.first());
        values.set(
            "first",
            FluentValue::from(args.get(1).map(text_of).unwrap_or_default()),
        );
        values.set(
            "second",
            FluentValue::from(args.get(2).map(text_of).unwrap_or_default()),
        );
        format(&grammar_basic, "zzzz-conjugate-basic", &values)
    });
    let grammar_counter = Arc::clone(grammar);
    let _ = bundle.add_function("COUNTER", move |args, _| {
        if let Some(counter) = args
            .first()
            .and_then(entity_of)
            .and_then(|e| e.attributes.get("counter"))
        {
            return string(counter.clone());
        }
        format(&grammar_counter, "zzzz-counter-default", &FluentArgs::new())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indefinite_articles_follow_robust_rules() {
        for (word, article) in [
            ("apple", "an"),
            ("crowbar", "a"),
            ("hour", "an"),
            ("honest man", "an"),
            ("European", "a"),
            ("one", "a"),
            ("unicorn", "a"),
            ("uninformed", "an"),
            ("user", "a"),
            ("FBI agent", "an"),
            ("UFO", "a"),
            ("x", "an"),
            ("b", "a"),
            ("yttrium", "an"),
            ("yellow", "a"),
            ("SMES", "an"),
            ("", "an"),
        ] {
            assert_eq!(indefinite_article(word), article, "{word}");
        }
    }

    #[test]
    fn grammar_attributes() {
        let mut grammar = Grammar::default();
        grammar.set_gender(Some("Female"));
        grammar.set_proper(Some(true));
        assert_eq!(grammar.gender(), Some("female"));
        assert_eq!(grammar.proper(), Some(true));
        grammar.set_proper(None);
        assert_eq!(grammar.proper(), None);
        let entity = EntityArg::new("Urist").with("gender", "Male");
        assert_eq!(entity.gender(), "male");
        assert!(!entity.proper());
        assert_eq!(escape_text("[b]\\"), "\\[b]\\\\");
        assert_eq!(escape_parameter("a\"b"), "a\\\"b");
    }
}
