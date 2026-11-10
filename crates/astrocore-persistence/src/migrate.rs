use serde_json::Value;

use crate::PersistError;

/// Текущая версия формата файла снимка.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

type Step = fn(Value) -> Result<Value, String>;

/// Цепочка миграций формата снимка: шаг `from` переводит документ версии `from`
/// в версию `from + 1`. Новая версия формата = новый шаг + golden-фикстура старого
/// файла в тестах (правило 5 раздела 26: схемы не меняются без миграции).
pub struct Migrator {
    target: u32,
    steps: Vec<(u32, Step)>,
}

impl Migrator {
    pub fn new(target: u32) -> Self {
        Self {
            target,
            steps: Vec::new(),
        }
    }

    /// Миграции до текущей версии формата. Пока существует только версия 1.
    pub fn current() -> Self {
        Self::new(SNAPSHOT_FORMAT_VERSION)
    }

    #[must_use]
    pub fn step(mut self, from: u32, f: Step) -> Self {
        self.steps.push((from, f));
        self
    }

    pub fn migrate(&self, mut doc: Value) -> Result<Value, PersistError> {
        let mut version = doc
            .get("format_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| PersistError::Migration("в снимке нет format_version".into()))?;
        let target = u64::from(self.target);
        if version > target {
            return Err(PersistError::Migration(format!(
                "снимок версии {version} новее поддерживаемой {target}"
            )));
        }
        while version < target {
            let step = self
                .steps
                .iter()
                .find(|(from, _)| u64::from(*from) == version)
                .ok_or_else(|| {
                    PersistError::Migration(format!("нет миграции с версии {version}"))
                })?;
            doc = (step.1)(doc).map_err(PersistError::Migration)?;
            version += 1;
            doc["format_version"] = Value::from(version);
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn current_version_is_untouched() {
        let doc = json!({"format_version": 1, "x": 1});
        assert_eq!(Migrator::current().migrate(doc.clone()).unwrap(), doc);
    }

    #[test]
    fn steps_run_in_order() {
        let m = Migrator::new(3)
            .step(1, |mut d| {
                d["a"] = json!(1);
                Ok(d)
            })
            .step(2, |mut d| {
                d["b"] = json!(d["a"].as_i64().unwrap() + 1);
                Ok(d)
            });
        let out = m.migrate(json!({"format_version": 1})).unwrap();
        assert_eq!(out, json!({"format_version": 3, "a": 1, "b": 2}));
    }

    #[test]
    fn missing_step_newer_version_and_failures_are_errors() {
        assert!(
            Migrator::new(2)
                .migrate(json!({"format_version": 1}))
                .is_err()
        );
        assert!(
            Migrator::current()
                .migrate(json!({"format_version": 7}))
                .is_err()
        );
        assert!(Migrator::current().migrate(json!({"nothing": 1})).is_err());
        let failing = Migrator::new(2).step(1, |_| Err("boom".into()));
        assert!(matches!(
            failing.migrate(json!({"format_version": 1})),
            Err(PersistError::Migration(m)) if m == "boom"
        ));
    }
}
