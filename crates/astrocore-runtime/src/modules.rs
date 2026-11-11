use std::collections::{BTreeMap, BTreeSet};

use astrocore_core::{NetworkEntityId, ObjectId};
use astrocore_math::ChunkCoord;
use astrocore_world::{World, WorldCommand};

/// Универсальные события движка.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    Interact {
        actor: ObjectId,
        target: ObjectId,
    },
    Collision {
        a: ObjectId,
        b: ObjectId,
    },
    PortalChanged {
        element: ObjectId,
        open: bool,
    },
    EntitySpawned {
        id: ObjectId,
    },
    ComponentChanged {
        id: ObjectId,
        component: String,
    },
    /// Игрок отключился; `pos` — его последняя позиция (например, чтобы выбросить предметы).
    PlayerLeft {
        actor: NetworkEntityId,
        pos: astrocore_math::glam::Vec3,
    },
    ChunkLoaded {
        chunk: ChunkCoord,
    },
    /// Игровая команда клиента (`ClientMessage::GameCommand`): вид и непрозрачные байты — их смысл
    /// знает только игровой модуль. Движок проверяет лишь размеры и частоту.
    Command {
        actor: NetworkEntityId,
        kind: String,
        data: Vec<u8>,
    },
}

/// Контекст вызова: чтение мира и накопление команд.
pub struct HookContext<'a> {
    pub world: &'a World,
    positions: &'a BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
    physics: Option<&'a astrocore_physics::BoxWorld>,
    commands: Vec<WorldCommand>,
    components: Vec<ComponentUpdate>,
    log: Vec<String>,
}

/// Значение реплицируемого вида данных, которое модуль просит установить сущности.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentUpdate {
    pub net: NetworkEntityId,
    pub kind: String,
    pub data: Vec<u8>,
}

impl<'a> HookContext<'a> {
    pub fn new(world: &'a World) -> Self {
        static NONE: std::sync::OnceLock<BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>> =
            std::sync::OnceLock::new();
        Self::with_positions(world, NONE.get_or_init(BTreeMap::new))
    }

    pub fn with_positions(
        world: &'a World,
        positions: &'a BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
    ) -> Self {
        Self {
            world,
            positions,
            physics: None,
            commands: Vec::new(),
            components: Vec::new(),
            log: Vec::new(),
        }
    }

    /// Установить реплицируемые данные сущности; вид должен быть зарегистрирован на сервере.
    pub fn set_component(&mut self, net: NetworkEntityId, kind: impl Into<String>, data: Vec<u8>) {
        self.components.push(ComponentUpdate {
            net,
            kind: kind.into(),
            data,
        });
    }

    /// Подключить коллизию мира: модули смогут проверять, что путь между точками свободен.
    #[must_use]
    pub fn with_physics(mut self, physics: &'a astrocore_physics::BoxWorld) -> Self {
        self.physics = Some(physics);
        self
    }

    /// Расстояние от `from` до `to`, на котором путь упирается в преграду (стену, закрытую дверь);
    /// без коллизии или без преград — полная длина. Аналог `UnobstructedDistance` в SS14.
    pub fn unobstructed_distance(
        &self,
        from: astrocore_math::glam::Vec3,
        to: astrocore_math::glam::Vec3,
    ) -> f32 {
        use astrocore_physics::SpatialQuery;
        let delta = to - from;
        let len = delta.length();
        let Some(bw) = self.physics else {
            return len;
        };
        if len < 1e-6 {
            return 0.0;
        }
        bw.raycast(from, delta / len, len)
            .map_or(len, |h| h.distance.min(len))
    }

    /// Свободен ли путь от `from` до `to`.
    pub fn is_unobstructed(
        &self,
        from: astrocore_math::glam::Vec3,
        to: astrocore_math::glam::Vec3,
    ) -> bool {
        self.unobstructed_distance(from, to) >= (to - from).length() - 1e-4
    }

    /// Позиция (ступни) подключённого игрока на момент вызова.
    pub fn position_of(&self, net: NetworkEntityId) -> Option<astrocore_math::glam::Vec3> {
        self.positions.get(&net).copied()
    }

    /// Забрать накопленное: команды мира, значения данных и журнал (для проверки модулей без
    /// сервера и сети).
    pub fn into_output(self) -> HookOutput {
        HookOutput {
            commands: self.commands,
            components: self.components,
            log: self.log,
        }
    }

    /// Запросить изменение мира; применит движок с валидацией.
    pub fn request(&mut self, command: WorldCommand) {
        self.commands.push(command);
    }

    pub fn log(&mut self, message: impl Into<String>) {
        self.log.push(message.into());
    }
}

/// Результат обработки события всеми модулями.
#[derive(Debug, Default, PartialEq)]
pub struct HookOutput {
    pub commands: Vec<WorldCommand>,
    pub components: Vec<ComponentUpdate>,
    pub log: Vec<String>,
}

pub trait GameModule: Send {
    /// Уникальное имя модуля, например `example.weather`.
    fn id(&self) -> &str;

    /// Модули, которые должны получить событие раньше этого.
    fn depends_on(&self) -> &[&'static str] {
        &[]
    }

    fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent);
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModuleError {
    #[error("модуль {0:?} уже зарегистрирован")]
    Duplicate(String),
    #[error("модуль {module:?} зависит от отсутствующего {missing:?}")]
    MissingDependency { module: String, missing: String },
    #[error("циклическая зависимость модулей: {0}")]
    Cycle(String),
}

/// Хост модулей: порядок вызова определяется зависимостями.
#[derive(Default)]
pub struct ModuleHost {
    modules: Vec<Box<dyn GameModule>>,
    order: Vec<usize>,
}

impl ModuleHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, module: Box<dyn GameModule>) -> Result<(), ModuleError> {
        if self.modules.iter().any(|m| m.id() == module.id()) {
            return Err(ModuleError::Duplicate(module.id().to_string()));
        }
        self.modules.push(module);
        Ok(())
    }

    /// Проверить зависимости и зафиксировать порядок (топологическая сортировка,
    /// при равенстве — порядок регистрации).
    pub fn finalize(&mut self) -> Result<(), ModuleError> {
        let ids: BTreeMap<&str, usize> = self
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id(), i))
            .collect();
        for m in &self.modules {
            for dep in m.depends_on() {
                if !ids.contains_key(dep) {
                    return Err(ModuleError::MissingDependency {
                        module: m.id().to_string(),
                        missing: (*dep).to_string(),
                    });
                }
            }
        }
        let mut order = Vec::new();
        let mut done = BTreeSet::new();
        while order.len() < self.modules.len() {
            let before = order.len();
            for (i, m) in self.modules.iter().enumerate() {
                if !done.contains(&i) && m.depends_on().iter().all(|d| done.contains(&ids[d])) {
                    done.insert(i);
                    order.push(i);
                }
            }
            if order.len() == before {
                let stuck: Vec<&str> = self
                    .modules
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !done.contains(i))
                    .map(|(_, m)| m.id())
                    .collect();
                return Err(ModuleError::Cycle(stuck.join(", ")));
            }
        }
        self.order = order;
        Ok(())
    }

    /// Разослать событие всем модулям в порядке зависимостей.
    pub fn dispatch(&mut self, world: &World, event: &HookEvent) -> HookOutput {
        self.dispatch_with(world, &BTreeMap::new(), None, event)
    }

    /// То же, но модули видят позиции игроков (`HookContext::position_of`).
    pub fn dispatch_with(
        &mut self,
        world: &World,
        positions: &BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
        physics: Option<&astrocore_physics::BoxWorld>,
        event: &HookEvent,
    ) -> HookOutput {
        let mut out = HookOutput::default();
        for &i in &self.order {
            let mut ctx = HookContext::with_positions(world, positions);
            if let Some(p) = physics {
                ctx = ctx.with_physics(p);
            }
            self.modules[i].handle(&mut ctx, event);
            out.commands.append(&mut ctx.commands);
            out.components.append(&mut ctx.components);
            out.log.append(&mut ctx.log);
        }
        out
    }

    pub fn module_ids(&self) -> Vec<&str> {
        self.order.iter().map(|&i| self.modules[i].id()).collect()
    }
}

#[cfg(test)]
mod tests {
    use astrocore_world::WorldConfig;

    use super::*;

    struct Recorder {
        id: &'static str,
        deps: &'static [&'static str],
    }

    impl GameModule for Recorder {
        fn id(&self) -> &str {
            self.id
        }
        fn depends_on(&self) -> &[&'static str] {
            self.deps
        }
        fn handle(&mut self, ctx: &mut HookContext<'_>, event: &HookEvent) {
            if let HookEvent::Interact { target, .. } = event {
                ctx.log(format!("{}:{}", self.id, target));
                if self.id == "a" {
                    ctx.request(WorldCommand::RemoveElement { id: *target });
                }
            }
        }
    }

    fn host(specs: &[(&'static str, &'static [&'static str])]) -> Result<ModuleHost, ModuleError> {
        let mut h = ModuleHost::new();
        for (id, deps) in specs {
            h.register(Box::new(Recorder { id, deps }))?;
        }
        h.finalize()?;
        Ok(h)
    }

    #[test]
    fn modules_run_in_dependency_order() {
        let h = host(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]).unwrap();
        assert_eq!(h.module_ids(), vec!["a", "b", "c"]);
    }

    #[test]
    fn registration_order_breaks_ties() {
        let h = host(&[("x", &[]), ("y", &[]), ("z", &["x"])]).unwrap();
        assert_eq!(h.module_ids(), vec!["x", "y", "z"]);
    }

    #[test]
    fn duplicates_missing_deps_and_cycles_are_rejected() {
        assert_eq!(
            host(&[("a", &[]), ("a", &[])]).err(),
            Some(ModuleError::Duplicate("a".into()))
        );
        assert!(matches!(
            host(&[("a", &["ghost"])]).err(),
            Some(ModuleError::MissingDependency { .. })
        ));
        assert!(matches!(
            host(&[("a", &["b"]), ("b", &["a"])]).err(),
            Some(ModuleError::Cycle(_))
        ));
    }

    #[test]
    fn dispatch_collects_commands_and_logs_in_order() {
        let world = World::new("t", WorldConfig::default()).unwrap();
        let mut h = host(&[("b", &["a"]), ("a", &[])]).unwrap();
        let target = ObjectId::from_u128(5);
        let out = h.dispatch(
            &world,
            &HookEvent::Interact {
                actor: ObjectId::from_u128(1),
                target,
            },
        );
        assert_eq!(out.log, vec![format!("a:{target}"), format!("b:{target}")]);
        assert_eq!(
            out.commands,
            vec![WorldCommand::RemoveElement { id: target }]
        );
        // События, которые модуль не обрабатывает, ничего не порождают.
        let quiet = h.dispatch(
            &world,
            &HookEvent::ChunkLoaded {
                chunk: ChunkCoord::new(0, 0),
            },
        );
        assert_eq!(quiet, HookOutput::default());
    }

    #[test]
    fn modules_cannot_mutate_the_world_directly() {
        // Компиляционная гарантия: HookContext отдаёт только &World.
        let world = World::new("t", WorldConfig::default()).unwrap();
        let ctx = HookContext::new(&world);
        assert_eq!(ctx.world.element_count(), 0);
    }
}
