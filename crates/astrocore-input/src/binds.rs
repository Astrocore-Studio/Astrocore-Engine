//! Назначение клавиш: файл `keybinds.yml` SS14, сочетания, приоритеты и контексты.
//! Образец — `InputManager` и `InputContextContainer` Robust @ 1afd692.

use std::collections::{HashMap, HashSet};

use astrocore_data::{DataError, DataNode, from_node, to_node, to_yaml_string};
use serde::{Deserialize, Serialize};

/// Как назначение реагирует на клавишу (`KeyBindingType` Robust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BindKind {
    /// Нажатие и отпускание.
    #[default]
    Default,
    /// Каждое нажатие переключает состояние.
    Toggle,
    /// Состояние держится, пока клавиша нажата.
    State,
}

/// Состояние функции ввода.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundKeyState {
    Up,
    Down,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

/// Назначение: функция ввода и клавиша с модификаторами. Имена клавиш — как в Robust
/// (`W`, `Shift`, `MouseLeft`, `Num1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyBind {
    pub function: String,
    #[serde(rename = "type", default)]
    pub kind: BindKind,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod2: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod3: Option<String>,
    /// Нажатие сначала получает интерфейс под курсором.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_focus: bool,
    /// Удержание клавиши повторяет нажатие.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_repeat: bool,
    /// Вместе с этим сочетанием срабатывают назначения его частей.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_sub_combs: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,
}

impl KeyBind {
    /// Клавиши сочетания: основная и модификаторы.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.key.as_str())
            .chain(self.mod1.as_deref())
            .chain(self.mod2.as_deref())
            .chain(self.mod3.as_deref())
    }

    fn contains(&self, key: &str) -> bool {
        self.keys().any(|k| k == key)
    }

    fn same_combo(&self, other: &KeyBind) -> bool {
        self.keys().count() == other.keys().count() && self.keys().all(|key| other.contains(key))
    }

    /// Сочетание `self` — часть сочетания `other`.
    fn is_part_of(&self, other: &KeyBind) -> bool {
        self.keys().all(|key| other.contains(key))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyBindFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    binds: Vec<KeyBind>,
    /// Функции, с которых игрок снял все назначения.
    #[serde(default)]
    leave_empty: Vec<String>,
}

/// Контексты ввода: наборы функций, доступных в текущем режиме игры. Контекст наследует
/// функции родителя.
#[derive(Debug, Clone)]
pub struct InputContexts {
    contexts: HashMap<String, (Option<String>, HashSet<String>)>,
    active: String,
}

/// Имя общего контекста: он есть всегда и активен по умолчанию.
pub const COMMON_CONTEXT: &str = "common";

impl Default for InputContexts {
    fn default() -> Self {
        Self {
            contexts: HashMap::from([(COMMON_CONTEXT.to_owned(), (None, HashSet::new()))]),
            active: COMMON_CONTEXT.to_owned(),
        }
    }
}

impl InputContexts {
    /// Создаёт контекст с родителем. Существующий контекст остаётся как был.
    pub fn add(&mut self, name: &str, parent: &str) {
        self.contexts
            .entry(name.to_owned())
            .or_insert_with(|| (Some(parent.to_owned()), HashSet::new()));
    }

    /// Добавляет функции в контекст.
    pub fn add_functions<'a>(
        &mut self,
        context: &str,
        functions: impl IntoIterator<Item = &'a str>,
    ) {
        if let Some((_, set)) = self.contexts.get_mut(context) {
            set.extend(functions.into_iter().map(str::to_owned));
        }
    }

    /// Делает контекст активным. Возвращает `false`, если такого контекста нет.
    pub fn set_active(&mut self, name: &str) -> bool {
        let known = self.contexts.contains_key(name);
        if known {
            self.active = name.to_owned();
        }
        known
    }

    pub fn active(&self) -> &str {
        &self.active
    }

    /// Функция доступна в активном контексте или его предках.
    pub fn allows(&self, function: &str) -> bool {
        let mut current = Some(self.active.as_str());
        // Глубина ограничена числом контекстов: цикл в родителях не зациклит поиск.
        for _ in 0..=self.contexts.len() {
            let Some((parent, functions)) = current.and_then(|name| self.contexts.get(name)) else {
                return false;
            };
            if functions.contains(function) {
                return true;
            }
            current = parent.as_deref();
        }
        false
    }
}

/// Изменение состояния функции ввода после нажатия или отпускания клавиши.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionChange {
    pub function: String,
    pub state: BoundKeyState,
    pub can_focus: bool,
}

/// Назначения клавиш и их текущее состояние.
#[derive(Debug, Clone, Default)]
pub struct KeyBindings {
    /// Назначения по убыванию размера сочетания и приоритета, признак «нажато».
    binds: Vec<(KeyBind, bool)>,
    /// Назначения по умолчанию: нужны, чтобы сохранять только изменения игрока.
    defaults: Vec<KeyBind>,
    /// Функции, с которых игрок снял назначения.
    emptied: HashSet<String>,
    pressed: HashSet<String>,
}

impl KeyBindings {
    fn sort(&mut self) {
        self.binds.sort_by(|(a, _), (b, _)| {
            (b.keys().count(), b.priority).cmp(&(a.keys().count(), a.priority))
        });
    }

    /// Загружает назначения по умолчанию (`keybinds.yml` из ресурсов).
    pub fn load_defaults(&mut self, node: &DataNode) -> Result<(), DataError> {
        let file: KeyBindFile = from_node(node)?;
        self.defaults = file.binds.clone();
        self.binds = file.binds.into_iter().map(|bind| (bind, false)).collect();
        self.emptied.clear();
        self.sort();
        Ok(())
    }

    /// Накладывает назначения игрока: они заменяют назначения тех же функций.
    pub fn load_user(&mut self, node: &DataNode) -> Result<(), DataError> {
        let file: KeyBindFile = from_node(node)?;
        let replaced: HashSet<&str> = file
            .binds
            .iter()
            .map(|bind| bind.function.as_str())
            .chain(file.leave_empty.iter().map(String::as_str))
            .collect();
        self.binds
            .retain(|(bind, _)| !replaced.contains(bind.function.as_str()));
        self.emptied.extend(file.leave_empty);
        self.binds
            .extend(file.binds.into_iter().map(|bind| (bind, false)));
        self.sort();
        Ok(())
    }

    /// Назначения функции.
    pub fn binds_of<'a>(&'a self, function: &'a str) -> impl Iterator<Item = &'a KeyBind> {
        self.binds
            .iter()
            .map(|(bind, _)| bind)
            .filter(move |bind| bind.function == function)
    }

    /// Заменяет назначения функции. Пустой список снимает их.
    pub fn rebind(&mut self, function: &str, binds: Vec<KeyBind>) {
        self.binds.retain(|(bind, _)| bind.function != function);
        if binds.is_empty() {
            self.emptied.insert(function.to_owned());
        } else {
            self.emptied.remove(function);
        }
        self.binds.extend(
            binds
                .into_iter()
                .filter(|bind| bind.function == function)
                .map(|bind| (bind, false)),
        );
        self.sort();
    }

    /// Возвращает функции к назначениям по умолчанию.
    pub fn reset(&mut self, function: &str) {
        let defaults: Vec<KeyBind> = self
            .defaults
            .iter()
            .filter(|bind| bind.function == function)
            .cloned()
            .collect();
        self.rebind(function, defaults);
        self.emptied.remove(function);
    }

    /// Изменения игрока в формате `keybinds.yml`: только функции, назначения которых
    /// отличаются от назначений по умолчанию.
    pub fn user_yaml(&self) -> String {
        let functions: HashSet<&str> = self
            .binds
            .iter()
            .map(|(bind, _)| bind.function.as_str())
            .chain(self.defaults.iter().map(|bind| bind.function.as_str()))
            .collect();
        let mut changed: Vec<&str> = functions
            .into_iter()
            .filter(|function| {
                let now: Vec<&KeyBind> = self.binds_of(function).collect();
                let default: Vec<&KeyBind> = self
                    .defaults
                    .iter()
                    .filter(|b| b.function == *function)
                    .collect();
                now.len() != default.len() || now.iter().any(|bind| !default.contains(bind))
            })
            .collect();
        changed.sort_unstable();
        let mut leave_empty: Vec<String> = self.emptied.iter().cloned().collect();
        leave_empty.sort_unstable();
        let file = KeyBindFile {
            version: 1,
            binds: changed
                .iter()
                .flat_map(|function| self.binds_of(function).cloned())
                .collect(),
            leave_empty,
        };
        to_node(&file)
            .map(|node| to_yaml_string(&node))
            .unwrap_or_default()
    }

    fn all_pressed(&self, bind: &KeyBind) -> bool {
        bind.keys().all(|key| self.pressed.contains(key))
    }

    /// Клавиша нажата. Возвращает изменения функций по порядку приоритета. Срабатывает
    /// самое большое сочетание с этой клавишей; его части молчат, если сочетание не
    /// разрешает их (`allowSubCombs`).
    pub fn key_down(&mut self, key: &str, contexts: &InputContexts) -> Vec<FunctionChange> {
        let repeat = !self.pressed.insert(key.to_owned());
        let mut matched: Option<KeyBind> = None;
        let mut allow_parts = false;
        let mut down: Vec<usize> = Vec::new();
        let mut silenced: Vec<usize> = Vec::new();
        for (index, (bind, _)) in self.binds.iter().enumerate() {
            if !contexts.allows(&bind.function) || !self.all_pressed(bind) {
                continue;
            }
            match &matched {
                None if bind.contains(key) => {
                    matched = Some(bind.clone());
                    allow_parts |= bind.allow_sub_combs;
                    down.push(index);
                }
                Some(combo) if bind.same_combo(combo) => {
                    allow_parts |= bind.allow_sub_combs;
                    down.push(index);
                }
                Some(combo) if bind.is_part_of(combo) => {
                    if allow_parts {
                        down.push(index);
                    } else {
                        silenced.push(index);
                    }
                }
                _ => {}
            }
        }
        let mut changes = Vec::new();
        for index in silenced {
            self.release(index, &mut changes);
        }
        for index in down {
            let (bind, is_down) = &mut self.binds[index];
            let state = match bind.kind {
                BindKind::Toggle if repeat => continue,
                BindKind::Toggle => {
                    *is_down = !*is_down;
                    if *is_down {
                        BoundKeyState::Down
                    } else {
                        BoundKeyState::Up
                    }
                }
                BindKind::State | BindKind::Default => {
                    if *is_down && !(repeat && bind.can_repeat) {
                        continue;
                    }
                    *is_down = true;
                    BoundKeyState::Down
                }
            };
            changes.push(FunctionChange {
                function: bind.function.clone(),
                state,
                can_focus: bind.can_focus,
            });
        }
        changes
    }

    fn release(&mut self, index: usize, changes: &mut Vec<FunctionChange>) {
        let (bind, is_down) = &mut self.binds[index];
        if *is_down && bind.kind != BindKind::Toggle {
            *is_down = false;
            changes.push(FunctionChange {
                function: bind.function.clone(),
                state: BoundKeyState::Up,
                can_focus: bind.can_focus,
            });
        }
    }

    /// Клавиша отпущена: нажатые назначения с этой клавишей отпускаются.
    pub fn key_up(&mut self, key: &str) -> Vec<FunctionChange> {
        self.pressed.remove(key);
        let mut changes = Vec::new();
        for index in 0..self.binds.len() {
            if self.binds[index].0.contains(key) {
                self.release(index, &mut changes);
            }
        }
        changes
    }

    /// Отпускает всё: окно потеряло фокус.
    pub fn release_all(&mut self) -> Vec<FunctionChange> {
        self.pressed.clear();
        let mut changes = Vec::new();
        for index in 0..self.binds.len() {
            self.release(index, &mut changes);
        }
        changes
    }
}
