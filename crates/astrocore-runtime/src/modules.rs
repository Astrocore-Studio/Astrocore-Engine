use std::collections::{BTreeMap, BTreeSet};

use astrocore_core::{NetworkEntityId, ObjectId};

use crate::doors::DoorRegistry;
use astrocore_math::ChunkCoord;
use astrocore_world::{MapDef, MapId, World, WorldCommand};

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
    /// Прошёл тик сервера: номер тика и длительность тика, с. Приходит каждый тик после движения
    /// игроков и столкновений; модуль держит по нему свои таймеры.
    Tick {
        tick: u32,
        dt: f32,
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
    /// Мир карты, в рамках которой вызван хук (ADR-0013): карта игрока для его команд, карта тика
    /// для `Tick`. Команды `request` меняют этот мир.
    pub world: &'a World,
    /// Карта, в рамках которой вызван хук.
    map: MapId,
    /// Все карты сервера: номер → определение (имя, пауза, инициализация).
    map_defs: &'a BTreeMap<MapId, MapDef>,
    /// Карта каждого игрока; пусто — все на карте 0.
    player_maps: &'a BTreeMap<NetworkEntityId, MapId>,
    /// Миры всех карт (только чтение): снимок другой карты, `savemap`.
    worlds: &'a BTreeMap<MapId, &'a World>,
    map_requests: Vec<MapRequest>,
    positions: &'a BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
    physics: Option<&'a astrocore_physics::BoxWorld>,
    doors: Option<&'a DoorRegistry>,
    door_requests: Vec<DoorRequest>,
    commands: Vec<WorldCommand>,
    components: Vec<ComponentUpdate>,
    freezes: Vec<(NetworkEntityId, bool)>,
    ghosts: Vec<(NetworkEntityId, bool)>,
    speeds: Vec<(NetworkEntityId, f32)>,
    teleports: Vec<(NetworkEntityId, astrocore_math::glam::Vec3)>,
    log: Vec<String>,
}

/// Что модуль просит сделать с дверью. Дверью управляет движок: он же проверяет замок, меняет
/// коллизию и рассылает состояние всем клиентам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorAction {
    Open,
    Close,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorRequest {
    pub element: ObjectId,
    pub action: DoorAction,
    /// Действовать, даже если дверь заперта (взлом, ломик).
    pub force: bool,
}

/// Что модуль просит сделать с картами (ADR-0013).
#[derive(Debug, Clone)]
pub enum MapRequest {
    /// Новая карта со своим миром.
    Create { def: MapDef, world: Box<World> },
    /// Удалить карту; игроков с неё движок переносит на карту 0.
    Remove { id: MapId },
    /// Сменить имя или флаги карты (пауза, инициализация).
    Define(MapDef),
    /// Перенести игрока на другую карту в точку (ступни); клиент загружает новую карту.
    Move {
        net: NetworkEntityId,
        map: MapId,
        pos: astrocore_math::glam::Vec3,
    },
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
        static NO_DEFS: std::sync::OnceLock<BTreeMap<MapId, MapDef>> = std::sync::OnceLock::new();
        static NO_MAPS: std::sync::OnceLock<BTreeMap<NetworkEntityId, MapId>> =
            std::sync::OnceLock::new();
        static NO_WORLDS: std::sync::OnceLock<BTreeMap<MapId, &'static World>> =
            std::sync::OnceLock::new();
        Self {
            world,
            map: MapId(0),
            map_defs: NO_DEFS.get_or_init(BTreeMap::new),
            player_maps: NO_MAPS.get_or_init(BTreeMap::new),
            worlds: NO_WORLDS.get_or_init(BTreeMap::new),
            map_requests: Vec::new(),
            positions,
            physics: None,
            doors: None,
            door_requests: Vec::new(),
            commands: Vec::new(),
            components: Vec::new(),
            freezes: Vec::new(),
            ghosts: Vec::new(),
            speeds: Vec::new(),
            teleports: Vec::new(),
            log: Vec::new(),
        }
    }

    /// Установить реплицируемые данные сущности; вид должен быть зарегистрирован на сервере.
    /// Заморозить игрока или снять заморозку; действует, пока модуль не снимет её.
    pub fn freeze(&mut self, net: NetworkEntityId, frozen: bool) {
        self.freezes.push((net, frozen));
    }

    /// Режим призрака: игрок летает сквозь стены (по плоскости, на своей высоте).
    pub fn set_ghost(&mut self, net: NetworkEntityId, ghost: bool) {
        self.ghosts.push((net, ghost));
    }

    /// Задать множитель скорости ходьбы игрока (замедление или ускорение).
    pub fn set_speed(&mut self, net: NetworkEntityId, factor: f32) {
        self.speeds.push((net, factor));
    }

    /// Поставить игрока в точку (ступни).
    pub fn teleport(&mut self, net: NetworkEntityId, pos: astrocore_math::glam::Vec3) {
        self.teleports.push((net, pos));
    }

    pub fn set_component(&mut self, net: NetworkEntityId, kind: impl Into<String>, data: Vec<u8>) {
        self.components.push(ComponentUpdate {
            net,
            kind: kind.into(),
            data,
        });
    }

    /// Подключить реестр дверей: модули смогут читать их состояние.
    #[must_use]
    pub fn with_doors(mut self, doors: &'a DoorRegistry) -> Self {
        self.doors = Some(doors);
        self
    }

    /// Открыта ли дверь; `None`, если двери нет или реестр не подключён.
    pub fn door_open(&self, element: ObjectId) -> Option<bool> {
        self.doors?.get(element).map(|d| d.open)
    }

    /// Заперта ли дверь.
    pub fn door_locked(&self, element: ObjectId) -> Option<bool> {
        self.doors?.get(element).map(|d| d.locked)
    }

    /// Попросить движок изменить состояние двери.
    pub fn request_door(&mut self, element: ObjectId, action: DoorAction, force: bool) {
        self.door_requests.push(DoorRequest {
            element,
            action,
            force,
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

    /// Подключить карты (ADR-0013): карта вызова, определения всех карт и карты игроков.
    #[must_use]
    pub fn with_maps(mut self, scope: MapScope<'a>) -> Self {
        self.map = scope.map;
        self.map_defs = scope.defs;
        self.player_maps = scope.player_maps;
        self.worlds = scope.worlds;
        self
    }

    /// Мир карты (только чтение); для карты вызова — `world`.
    pub fn world_of(&self, map: MapId) -> Option<&World> {
        if map == self.map {
            Some(self.world)
        } else {
            self.worlds.get(&map).copied()
        }
    }

    /// Карта, в рамках которой вызван хук (её мир — `world`).
    pub fn map(&self) -> MapId {
        self.map
    }

    /// Определение карты; карта 0 есть всегда.
    pub fn map_def(&self, id: MapId) -> Option<MapDef> {
        self.map_defs
            .get(&id)
            .cloned()
            .or_else(|| (id == MapId(0)).then(MapDef::default_map))
    }

    /// Все карты сервера по номеру.
    pub fn maps(&self) -> Vec<MapDef> {
        let mut out: Vec<MapDef> = self.map_defs.values().cloned().collect();
        if !self.map_defs.contains_key(&MapId(0)) {
            out.insert(0, MapDef::default_map());
        }
        out
    }

    /// Стоит ли карта вызова (на паузе): её симуляции модуль не продвигает.
    pub fn paused(&self) -> bool {
        self.map_def(self.map).is_some_and(|m| m.paused)
    }

    /// Карта игрока.
    pub fn map_of(&self, net: NetworkEntityId) -> Option<MapId> {
        self.positions
            .contains_key(&net)
            .then(|| self.player_maps.get(&net).copied().unwrap_or(MapId(0)))
    }

    /// Игроки на карте вызова (по возрастанию сетевого идентификатора).
    pub fn players(&self) -> impl Iterator<Item = NetworkEntityId> + '_ {
        let here = self.map;
        self.positions
            .keys()
            .copied()
            .filter(move |n| self.player_maps.get(n).copied().unwrap_or(MapId(0)) == here)
    }

    /// Все подключённые игроки, на любой карте.
    pub fn all_players(&self) -> impl Iterator<Item = NetworkEntityId> + '_ {
        self.positions.keys().copied()
    }

    /// Позиция (ступни) подключённого игрока на момент вызова — в координатах его карты
    /// (карту даёт [`HookContext::map_of`]).
    pub fn position_of(&self, net: NetworkEntityId) -> Option<astrocore_math::glam::Vec3> {
        self.positions.get(&net).copied()
    }

    /// Создать карту со своим миром.
    pub fn create_map(&mut self, def: MapDef, world: World) {
        self.map_requests.push(MapRequest::Create {
            def,
            world: Box::new(world),
        });
    }

    /// Удалить карту (игроков с неё — на карту 0).
    pub fn remove_map(&mut self, id: MapId) {
        self.map_requests.push(MapRequest::Remove { id });
    }

    /// Сменить имя или флаги карты.
    pub fn define_map(&mut self, def: MapDef) {
        self.map_requests.push(MapRequest::Define(def));
    }

    /// Перенести игрока на карту `map` в точку `pos` её координат.
    pub fn move_to_map(
        &mut self,
        net: NetworkEntityId,
        map: MapId,
        pos: astrocore_math::glam::Vec3,
    ) {
        self.map_requests.push(MapRequest::Move { net, map, pos });
    }

    /// Забрать накопленное: команды мира, значения данных и журнал (для проверки модулей без
    /// сервера и сети).
    pub fn into_output(self) -> HookOutput {
        HookOutput {
            commands: self.commands,
            components: self.components,
            freezes: self.freezes,
            ghosts: self.ghosts,
            speeds: self.speeds,
            teleports: self.teleports,
            doors: self.door_requests,
            maps: self.map_requests,
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
#[derive(Debug, Default)]
pub struct HookOutput {
    pub commands: Vec<WorldCommand>,
    pub components: Vec<ComponentUpdate>,
    /// Просьбы заморозить (`true`) или освободить игрока: у замороженного команды движения
    /// не двигают персонажа (оглушение, критическое состояние).
    pub freezes: Vec<(NetworkEntityId, bool)>,
    /// Включить (`true`) или выключить режим призрака: полёт сквозь преграды без тяжести.
    pub ghosts: Vec<(NetworkEntityId, bool)>,
    /// Множитель скорости ходьбы игрока (1,0 — обычная); действует, пока модуль не изменит его.
    pub speeds: Vec<(NetworkEntityId, f32)>,
    /// Переставить игрока в точку (ступни); клиент получает новую позицию сразу.
    pub teleports: Vec<(NetworkEntityId, astrocore_math::glam::Vec3)>,
    pub doors: Vec<DoorRequest>,
    /// Просьбы о картах: создать, удалить, сменить флаги, перенести игрока.
    pub maps: Vec<MapRequest>,
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

/// Карта, в рамках которой рассылается событие, и сведения о картах сервера.
#[derive(Debug, Clone, Copy)]
pub struct MapScope<'a> {
    pub map: MapId,
    pub defs: &'a BTreeMap<MapId, MapDef>,
    pub player_maps: &'a BTreeMap<NetworkEntityId, MapId>,
    /// Миры остальных карт (только чтение).
    pub worlds: &'a BTreeMap<MapId, &'a World>,
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
        self.dispatch_with(world, &BTreeMap::new(), None, None, event)
    }

    /// То же, но модули видят позиции игроков (`HookContext::position_of`).
    pub fn dispatch_with(
        &mut self,
        world: &World,
        positions: &BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
        physics: Option<&astrocore_physics::BoxWorld>,
        doors: Option<&DoorRegistry>,
        event: &HookEvent,
    ) -> HookOutput {
        let (defs, maps, worlds) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        self.dispatch_on(
            MapScope {
                map: MapId(0),
                defs: &defs,
                player_maps: &maps,
                worlds: &worlds,
            },
            world,
            positions,
            physics,
            doors,
            event,
        )
    }

    /// То же в рамках карты (ADR-0013): `world` — мир карты `scope.map`.
    pub fn dispatch_on(
        &mut self,
        scope: MapScope<'_>,
        world: &World,
        positions: &BTreeMap<NetworkEntityId, astrocore_math::glam::Vec3>,
        physics: Option<&astrocore_physics::BoxWorld>,
        doors: Option<&DoorRegistry>,
        event: &HookEvent,
    ) -> HookOutput {
        let mut out = HookOutput::default();
        for &i in &self.order {
            let mut ctx = HookContext::with_positions(world, positions).with_maps(scope);
            if let Some(p) = physics {
                ctx = ctx.with_physics(p);
            }
            if let Some(d) = doors {
                ctx = ctx.with_doors(d);
            }
            self.modules[i].handle(&mut ctx, event);
            out.commands.append(&mut ctx.commands);
            out.components.append(&mut ctx.components);
            out.freezes.append(&mut ctx.freezes);
            out.ghosts.append(&mut ctx.ghosts);
            out.speeds.append(&mut ctx.speeds);
            out.teleports.append(&mut ctx.teleports);
            out.doors.append(&mut ctx.door_requests);
            out.maps.append(&mut ctx.map_requests);
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
        assert!(
            quiet.commands.is_empty()
                && quiet.components.is_empty()
                && quiet.maps.is_empty()
                && quiet.log.is_empty()
        );
    }

    #[test]
    fn modules_cannot_mutate_the_world_directly() {
        // Компиляционная гарантия: HookContext отдаёт только &World.
        let world = World::new("t", WorldConfig::default()).unwrap();
        let ctx = HookContext::new(&world);
        assert_eq!(ctx.world.element_count(), 0);
    }
}
