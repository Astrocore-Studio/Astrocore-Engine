//! Наследование прототипов на уровне дерева данных.
//!
//! Образец — `SerializationManager.Composition.cs` и `ComponentRegistrySerializer.PushInheritance`
//! Robust @ 1afd692. Поле потомка заменяет поле родителя; поля родителя, которых у потомка нет,
//! копируются. Поля «всегда наследовать» складываются: списки склеиваются, словари
//! объединяются. Поля «никогда не наследовать» от родителя не приходят.

use std::collections::{BTreeMap, HashMap};

use astrocore_data::{DataMap, DataNode, Value};

/// Предел глубины цепочки наследования.
pub const MAX_INHERITANCE_DEPTH: usize = 64;

/// Правила наследования полей одного вида записи.
#[derive(Debug, Clone, Copy, Default)]
pub struct FieldRules {
    /// Поля, которые складываются с родительскими (`AlwaysPushInheritance`).
    pub always: &'static [&'static str],
    /// Поля, которые от родителя не наследуются (`NeverPushInheritance`).
    pub never: &'static [&'static str],
}

/// Складывает значение поля «всегда наследовать»: элементы потомка, затем родителя.
fn compose(child: &DataNode, parent: &DataNode) -> DataNode {
    match (&child.value, &parent.value) {
        (Value::Seq(own), Value::Seq(inherited)) => {
            let mut items = own.clone();
            items.extend(inherited.iter().cloned());
            DataNode {
                value: Value::Seq(items),
                tag: child.tag.clone(),
                line: child.line,
            }
        }
        (Value::Map(own), Value::Map(inherited)) => DataNode {
            value: Value::Map(merge_maps(own, inherited, FieldRules::default())),
            tag: child.tag.clone(),
            line: child.line,
        },
        _ => child.clone(),
    }
}

/// Отображение потомка, дополненное полями родителя.
pub fn merge_maps(child: &DataMap, parent: &DataMap, rules: FieldRules) -> DataMap {
    let mut result = child.clone();
    for (key, inherited) in parent.iter() {
        if rules.never.contains(&key) {
            continue;
        }
        match result.get_mut(key) {
            Some(own) if rules.always.contains(&key) => *own = compose(own, inherited),
            Some(_) => {}
            None => result.0.push((key.to_owned(), inherited.clone())),
        }
    }
    result
}

/// Узел потомка, дополненный узлом родителя. Не отображения не складываются.
pub fn merge_nodes(child: &DataNode, parent: &DataNode, rules: FieldRules) -> DataNode {
    match (&child.value, &parent.value) {
        (Value::Map(own), Value::Map(inherited)) => DataNode {
            value: Value::Map(merge_maps(own, inherited, rules)),
            tag: child.tag.clone(),
            line: child.line,
        },
        // Пустая запись (`- type: Foo` без полей) наследует все поля родителя.
        _ if child.is_null() => DataNode {
            value: parent.value.clone(),
            tag: child.tag.clone().or_else(|| parent.tag.clone()),
            line: child.line,
        },
        _ => child.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderError {
    UnknownParent { child: String, parent: String },
    Cycle(Vec<String>),
    TooDeep(String),
}

/// Порядок разрешения: родители раньше потомков; при равных условиях — по алфавиту.
/// Записи с ошибкой наследования в порядок не входят и возвращаются отдельно.
pub fn resolution_order<'a>(
    parents: &BTreeMap<&'a str, &'a [String]>,
) -> (Vec<&'a str>, Vec<OrderError>) {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Visiting,
        Done(usize),
        Failed,
    }

    struct Walk<'a, 'b> {
        parents: &'b BTreeMap<&'a str, &'a [String]>,
        state: HashMap<&'a str, State>,
        path: Vec<&'a str>,
        order: Vec<&'a str>,
        errors: Vec<OrderError>,
    }

    impl<'a> Walk<'a, '_> {
        fn visit(&mut self, id: &'a str) -> Option<usize> {
            match self.state.get(id) {
                Some(State::Done(depth)) => return Some(*depth),
                Some(State::Failed) => return None,
                Some(State::Visiting) => {
                    let start = self.path.iter().position(|&p| p == id).unwrap_or(0);
                    let mut cycle: Vec<String> =
                        self.path[start..].iter().map(|s| (*s).to_owned()).collect();
                    cycle.push(id.to_owned());
                    self.errors.push(OrderError::Cycle(cycle));
                    return None;
                }
                None => {}
            }
            self.state.insert(id, State::Visiting);
            self.path.push(id);
            let mut depth = Some(1);
            for parent in self.parents[id] {
                let Some((&key, _)) = self.parents.get_key_value(parent.as_str()) else {
                    self.errors.push(OrderError::UnknownParent {
                        child: id.to_owned(),
                        parent: parent.clone(),
                    });
                    depth = None;
                    continue;
                };
                match self.visit(key) {
                    Some(parent_depth) => depth = depth.map(|d| d.max(parent_depth + 1)),
                    None => depth = None,
                }
            }
            self.path.pop();
            if depth.is_some_and(|d| d > MAX_INHERITANCE_DEPTH) {
                self.errors.push(OrderError::TooDeep(id.to_owned()));
                depth = None;
            }
            match depth {
                Some(d) => {
                    self.state.insert(id, State::Done(d));
                    self.order.push(id);
                }
                None => {
                    self.state.insert(id, State::Failed);
                }
            }
            depth
        }
    }

    let mut walk = Walk {
        parents,
        state: HashMap::with_capacity(parents.len()),
        path: Vec::new(),
        order: Vec::with_capacity(parents.len()),
        errors: Vec::new(),
    };
    for &id in parents.keys() {
        walk.visit(id);
    }
    (walk.order, walk.errors)
}

#[cfg(test)]
mod tests {
    use astrocore_data::parse_yaml;

    use super::*;

    #[test]
    fn child_fields_win_and_parent_fields_fill_in() {
        let child = parse_yaml("name: child\ntags: [a]\nslots: {x: 1}\nabstract: true\n").unwrap();
        let parent =
            parse_yaml("name: parent\ndesc: text\ntags: [b]\nslots: {x: 2, y: 3}\nhidden: true\n")
                .unwrap();
        let rules = FieldRules {
            always: &["tags", "slots"],
            never: &["hidden"],
        };
        let merged = merge_nodes(&child, &parent, rules);
        let expected = parse_yaml(
            "name: child\ntags: [a, b]\nslots: {x: 1, y: 3}\nabstract: true\ndesc: text\n",
        )
        .unwrap();
        assert_eq!(merged, expected);

        let plain = merge_nodes(&child, &parent, FieldRules::default());
        assert_eq!(plain.get("tags"), child.get("tags"));
        assert!(plain.get("hidden").is_some());
    }

    #[test]
    fn order_puts_parents_first_and_reports_errors() {
        let none: &[String] = &[];
        let base = ["Base".to_owned()];
        let item = ["Item".to_owned()];
        let missing = ["Missing".to_owned()];
        let a = ["B".to_owned()];
        let b = ["A".to_owned()];
        let parents: BTreeMap<&str, &[String]> = [
            ("Crowbar", &item[..]),
            ("Item", &base[..]),
            ("Base", none),
            ("Orphan", &missing[..]),
            ("A", &a[..]),
            ("B", &b[..]),
        ]
        .into();
        let (order, errors) = resolution_order(&parents);
        assert_eq!(order, ["Base", "Item", "Crowbar"]);
        assert!(errors.contains(&OrderError::UnknownParent {
            child: "Orphan".into(),
            parent: "Missing".into()
        }));
        assert!(errors.iter().any(|e| matches!(e, OrderError::Cycle(_))));
    }
}
