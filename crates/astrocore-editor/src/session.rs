use astrocore_core::{ObjectId, Revision};
use astrocore_world::{FloorDef, World, WorldCommand, WorldError, WorldOp};

/// Максимальная глубина истории по умолчанию.
pub const DEFAULT_HISTORY_DEPTH: usize = 1000;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("мир отклонил команду: {0}")]
    World(#[from] WorldError),
    #[error("нечего отменять")]
    NothingToUndo,
    #[error("нечего повторять")]
    NothingToRedo,
    #[error(
        "мир изменился вне редактора (ревизия {actual:?}, ожидалась {expected:?}): история недействительна"
    )]
    ExternalChange {
        expected: Revision,
        actual: Revision,
    },
}

/// Обратимая транзакция: прямые команды и команды, возвращающие мир в прежнее состояние.
#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    pub label: String,
    pub forward: Vec<WorldCommand>,
    pub inverse: Vec<WorldCommand>,
}

/// Сессия редактирования копии мира.
#[derive(Debug)]
pub struct EditSession {
    world: World,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    max_depth: usize,
    /// Операции, применённые в сессии (для журнала проекта или отправки на сервер).
    journal: Vec<WorldOp>,
    /// Ревизия мира после последней операции сессии: любое другое изменение — внешнее.
    expected_revision: Revision,
}

impl EditSession {
    pub fn new(world: World) -> Self {
        let expected_revision = world.revision();
        Self {
            world,
            undo: Vec::new(),
            redo: Vec::new(),
            max_depth: DEFAULT_HISTORY_DEPTH,
            journal: Vec::new(),
            expected_revision,
        }
    }

    pub fn with_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth.max(1);
        self
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn into_world(self) -> World {
        self.world
    }

    pub fn journal(&self) -> &[WorldOp] {
        &self.journal
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    pub fn history_len(&self) -> usize {
        self.undo.len()
    }

    /// Обратная команда для `command`, вычисленная **до** применения.
    fn inverse_of(&self, command: &WorldCommand) -> Option<WorldCommand> {
        match command {
            WorldCommand::PlaceElement(e) => Some(WorldCommand::RemoveElement { id: e.id }),
            WorldCommand::RemoveElement { id } => self
                .world
                .element(*id)
                .map(|e| WorldCommand::PlaceElement(e.clone())),
            WorldCommand::DefineFloor(def) => Some(WorldCommand::RemoveFloor { id: def.id }),
            WorldCommand::RemoveFloor { id } => self
                .world
                .floor(*id)
                .map(|f: &FloorDef| WorldCommand::DefineFloor(f.clone())),
        }
    }

    fn check_external(&self) -> Result<(), EditError> {
        if self.world.revision() != self.expected_revision {
            return Err(EditError::ExternalChange {
                expected: self.expected_revision,
                actual: self.world.revision(),
            });
        }
        Ok(())
    }

    /// Применить список команд как одну транзакцию (всё или ничего).
    /// Новая транзакция сбрасывает стек «повторить».
    pub fn execute(
        &mut self,
        label: impl Into<String>,
        commands: Vec<WorldCommand>,
    ) -> Result<(), EditError> {
        self.check_external()?;
        let mut applied_inverse: Vec<WorldCommand> = Vec::new();
        let mut ops = Vec::new();
        for cmd in &commands {
            let inverse = self.inverse_of(cmd);
            match self.world.apply(cmd.clone()) {
                Ok(op) => {
                    ops.push(op);
                    if let Some(inv) = inverse {
                        applied_inverse.push(inv);
                    }
                }
                Err(e) => {
                    // Откат уже применённых команд этой транзакции.
                    for inv in applied_inverse.into_iter().rev() {
                        if let Ok(op) = self.world.apply(inv) {
                            ops.push(op);
                        }
                    }
                    self.journal.extend(ops);
                    self.expected_revision = self.world.revision();
                    return Err(e.into());
                }
            }
        }
        applied_inverse.reverse();
        self.expected_revision = self.world.revision();
        self.journal.extend(ops);
        if commands.is_empty() {
            return Ok(());
        }
        self.redo.clear();
        self.undo.push(Transaction {
            label: label.into(),
            forward: commands,
            inverse: applied_inverse,
        });
        if self.undo.len() > self.max_depth {
            self.undo.remove(0);
        }
        Ok(())
    }

    pub fn undo(&mut self) -> Result<String, EditError> {
        self.check_external()?;
        let t = self.undo.pop().ok_or(EditError::NothingToUndo)?;
        if let Err(e) = self.run(&t.inverse) {
            self.undo.push(t);
            return Err(e);
        }
        let label = t.label.clone();
        self.redo.push(t);
        Ok(label)
    }

    pub fn redo(&mut self) -> Result<String, EditError> {
        self.check_external()?;
        let t = self.redo.pop().ok_or(EditError::NothingToRedo)?;
        if let Err(e) = self.run(&t.forward) {
            self.redo.push(t);
            return Err(e);
        }
        let label = t.label.clone();
        self.undo.push(t);
        Ok(label)
    }

    fn run(&mut self, commands: &[WorldCommand]) -> Result<(), EditError> {
        let mut applied: Vec<(WorldCommand, WorldCommand)> = Vec::new(); // (обратная команда для отката)
        for cmd in commands {
            let inverse = self.inverse_of(cmd);
            match self.world.apply(cmd.clone()) {
                Ok(op) => {
                    self.journal.push(op);
                    if let Some(inv) = inverse {
                        applied.push((cmd.clone(), inv));
                    }
                }
                Err(e) => {
                    for (_, inv) in applied.into_iter().rev() {
                        if let Ok(op) = self.world.apply(inv) {
                            self.journal.push(op);
                        }
                    }
                    self.expected_revision = self.world.revision();
                    return Err(e.into());
                }
            }
        }
        self.expected_revision = self.world.revision();
        Ok(())
    }

    /// Забрать dirty-регионы для пересборки предпросмотра.
    pub fn take_dirty(&mut self) -> Vec<astrocore_world::DirtyRegion> {
        self.world.take_dirty()
    }

    /// Элементы мира, о которых знает сессия (для выделения).
    pub fn exists(&self, id: ObjectId) -> bool {
        self.world.element(id).is_some()
    }

    /// Сообщить сессии о намеренной внешней правке мира (например, применённый патч): история сбрасывается.
    pub fn reset_history_after_external_change(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.expected_revision = self.world.revision();
    }

    /// Применить к миру сессии правку «снаружи» (например, из `World`), минуя историю.
    /// История после этого недействительна, поэтому сбрасывается.
    pub fn apply_untracked(&mut self, command: WorldCommand) -> Result<WorldOp, EditError> {
        let op = self.world.apply(command)?;
        self.journal.push(op.clone());
        self.reset_history_after_external_change();
        Ok(op)
    }
}
