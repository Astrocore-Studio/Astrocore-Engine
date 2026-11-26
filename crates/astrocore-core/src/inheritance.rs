use std::collections::BTreeMap;

pub const MAX_INHERITANCE_DEPTH: usize = 32;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InheritanceError {
    #[error("прототип {child:?}: неизвестный родитель {parent:?}")]
    UnknownParent { child: String, parent: String },
    #[error("цикл наследования: {}", .0.join(" -> "))]
    Cycle(Vec<String>),
    #[error("наследование {0:?} глубже {MAX_INHERITANCE_DEPTH}")]
    TooDeep(String),
}

/// Возвращает порядок разрешения каталога. Родители всегда идут раньше потомков.
/// Правила слияния полей определяет формат каталога.
pub fn parent_order<T>(
    definitions: &BTreeMap<String, T>,
    parents: fn(&T) -> &[String],
) -> Result<Vec<&str>, InheritanceError> {
    let mut visit = Visit {
        definitions,
        parents,
        depths: BTreeMap::new(),
        path: Vec::new(),
        ordered: Vec::with_capacity(definitions.len()),
    };
    for (id, definition) in definitions {
        visit.walk(id, definition)?;
    }
    Ok(visit.ordered)
}

struct Visit<'a, T> {
    definitions: &'a BTreeMap<String, T>,
    parents: fn(&T) -> &[String],
    depths: BTreeMap<&'a str, usize>,
    path: Vec<&'a str>,
    ordered: Vec<&'a str>,
}

impl<'a, T> Visit<'a, T> {
    fn walk(&mut self, id: &'a str, definition: &'a T) -> Result<usize, InheritanceError> {
        if let Some(&depth) = self.depths.get(id) {
            return Ok(depth);
        }
        if let Some(index) = self.path.iter().position(|&entry| entry == id) {
            let cycle = self.path[index..]
                .iter()
                .copied()
                .chain([id])
                .map(str::to_owned);
            return Err(InheritanceError::Cycle(cycle.collect()));
        }
        if self.path.len() >= MAX_INHERITANCE_DEPTH {
            return Err(InheritanceError::TooDeep(id.to_owned()));
        }
        self.path.push(id);
        let mut depth = 1;
        for parent in (self.parents)(definition) {
            let Some((key, parent_definition)) = self.definitions.get_key_value(parent) else {
                return Err(InheritanceError::UnknownParent {
                    child: id.to_owned(),
                    parent: parent.clone(),
                });
            };
            depth = depth.max(1 + self.walk(key, parent_definition)?);
        }
        self.path.pop();
        if depth > MAX_INHERITANCE_DEPTH {
            return Err(InheritanceError::TooDeep(id.to_owned()));
        }
        self.depths.insert(id, depth);
        self.ordered.push(id);
        Ok(depth)
    }
}
