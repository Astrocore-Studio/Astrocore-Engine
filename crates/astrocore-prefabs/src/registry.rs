use std::collections::{BTreeMap, BTreeSet};

use astrocore_core::{PrefabNodeId, PrototypeId, SchemaVersion};
use serde::{Deserialize, Serialize};

use crate::model::*;

/// Максимальная суммарная глубина вложенности и наследования.
pub const MAX_PREFAB_DEPTH: usize = 16;
pub const PREFAB_SCHEMA_VERSION: SchemaVersion = SchemaVersion(1);

/// Файл библиотеки префабов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabLibraryFile {
    pub schema_version: SchemaVersion,
    pub prefabs: Vec<PrefabDef>,
}

#[derive(Debug, Clone, Default)]
pub struct PrefabRegistry {
    defs: BTreeMap<PrototypeId, PrefabDef>,
}

impl PrefabRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить определение. Порядок добавления не важен: связи проверяет [`validate`](Self::validate).
    pub fn insert(&mut self, def: PrefabDef) -> Result<(), PrefabError> {
        if self.defs.contains_key(&def.id) {
            return Err(PrefabError::DuplicatePrefab(def.id));
        }
        let mut seen = BTreeSet::new();
        for n in &def.nodes {
            if !seen.insert(n.id) {
                return Err(PrefabError::DuplicateNode {
                    prefab: def.id.clone(),
                    node: n.id,
                });
            }
            if let NodeContent::Element(t) = &n.content
                && (!t.thickness.is_finite() || t.thickness <= 0.0)
            {
                return Err(PrefabError::InvalidDimension(format!(
                    "{}: thickness={}",
                    def.id, t.thickness
                )));
            }
        }
        self.defs.insert(def.id.clone(), def);
        Ok(())
    }

    /// Заменить определение (обновление источника префаба).
    pub fn replace(&mut self, def: PrefabDef) -> Result<(), PrefabError> {
        self.defs.remove(&def.id);
        self.insert(def)
    }

    pub fn get(&self, id: &PrototypeId) -> Option<&PrefabDef> {
        self.defs.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &PrototypeId> {
        self.defs.keys()
    }

    /// Проверить все ссылки: наличие, циклы, версии, глубину.
    pub fn validate(&self) -> Result<(), PrefabError> {
        for id in self.defs.keys() {
            let mut stack = Vec::new();
            self.check(id, &mut stack)?;
        }
        Ok(())
    }

    fn check(&self, id: &PrototypeId, stack: &mut Vec<PrototypeId>) -> Result<(), PrefabError> {
        if stack.contains(id) {
            let chain: Vec<String> = stack.iter().chain([id]).map(ToString::to_string).collect();
            return Err(PrefabError::Cycle(chain.join(" → ")));
        }
        if stack.len() >= MAX_PREFAB_DEPTH {
            return Err(PrefabError::TooDeep(MAX_PREFAB_DEPTH));
        }
        let def = self
            .defs
            .get(id)
            .ok_or_else(|| PrefabError::Missing(id.clone()))?;
        stack.push(id.clone());
        if let Some(parent) = &def.parent {
            self.check(parent, stack)?;
        }
        for node in &def.nodes {
            if let NodeContent::Child(c) = &node.content {
                let child = self
                    .defs
                    .get(&c.prefab)
                    .ok_or_else(|| PrefabError::Missing(c.prefab.clone()))?;
                if let Some(required) = c.required_version
                    && required != child.version
                {
                    return Err(PrefabError::VersionMismatch {
                        prefab: id.clone(),
                        child: c.prefab.clone(),
                        required,
                        actual: child.version,
                    });
                }
                self.check(&c.prefab, stack)?;
            }
        }
        stack.pop();
        Ok(())
    }

    pub fn to_file(&self) -> PrefabLibraryFile {
        PrefabLibraryFile {
            schema_version: PREFAB_SCHEMA_VERSION,
            prefabs: self.defs.values().cloned().collect(),
        }
    }

    /// Загрузить библиотеку из недоверенного JSON с полной проверкой.
    pub fn from_json(text: &str) -> Result<Self, PrefabError> {
        let file: PrefabLibraryFile =
            serde_json::from_str(text).map_err(|e| PrefabError::File(e.to_string()))?;
        if file.schema_version != PREFAB_SCHEMA_VERSION {
            return Err(PrefabError::File(format!(
                "версия схемы {} не поддерживается (текущая {})",
                file.schema_version.0, PREFAB_SCHEMA_VERSION.0
            )));
        }
        let mut reg = Self::new();
        for def in file.prefabs {
            reg.insert(def)?;
        }
        reg.validate()?;
        Ok(reg)
    }
}

/// Изменение узла между двумя версиями определения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeChange {
    Added(PrefabNodeId),
    Removed(PrefabNodeId),
    Changed(PrefabNodeId),
}

/// Различия определений (для просмотра изменений и слияния).
pub fn diff_defs(old: &PrefabDef, new: &PrefabDef) -> Vec<NodeChange> {
    let a: BTreeMap<_, _> = old.nodes.iter().map(|n| (n.id, n)).collect();
    let b: BTreeMap<_, _> = new.nodes.iter().map(|n| (n.id, n)).collect();
    let mut out = Vec::new();
    for (id, n) in &a {
        match b.get(id) {
            None => out.push(NodeChange::Removed(*id)),
            Some(m) if m.content != n.content => out.push(NodeChange::Changed(*id)),
            Some(_) => {}
        }
    }
    out.extend(
        b.keys()
            .filter(|id| !a.contains_key(id))
            .map(|id| NodeChange::Added(*id)),
    );
    out
}
