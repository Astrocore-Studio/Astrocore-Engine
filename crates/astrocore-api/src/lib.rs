mod replication;

use std::collections::BTreeSet;

use astrocore_assets::{AssetEntry, AssetManifest};
use astrocore_core::{
    EntityError, EntityRegistry, ObjectId, PrototypeId, Revision, RuntimeEntityId,
};
use astrocore_math::Aabb;
use astrocore_math::glam::Vec3;
use astrocore_physics::{BoxWorld, Contact, RayHit, ShapeHit};
use astrocore_spatial::{Channel, RoomId, Topology, TopologyChange};
use astrocore_world::{World, WorldCommand, WorldError, WorldOp};

pub use astrocore_physics::SpatialQuery;
pub use astrocore_runtime::modules::{GameModule, HookContext, HookEvent, HookOutput, ModuleHost};
pub use replication::{
    Interest, ReplicatedKind, ReplicationChannel, ReplicationError, ReplicationRegistry,
};

/// Чтение и изменение канонического мира. Все изменения идут командами с валидацией.
pub trait WorldApi {
    fn world(&self) -> &World;

    /// Применить команду; результат — подтверждённая операция (запись журнала).
    fn submit(&mut self, command: WorldCommand) -> Result<WorldOp, WorldError>;

    fn revision(&self) -> Revision {
        self.world().revision()
    }
}

impl WorldApi for World {
    fn world(&self) -> &World {
        self
    }

    fn submit(&mut self, command: WorldCommand) -> Result<WorldOp, WorldError> {
        self.apply(command)
    }
}

/// Сущности рантайма с защитой от устаревших дескрипторов.
pub trait EntityApi {
    fn spawn_entity(&mut self, object: ObjectId) -> Result<RuntimeEntityId, EntityError>;
    fn despawn_entity(&mut self, id: RuntimeEntityId) -> Result<ObjectId, EntityError>;
    fn entity_of(&self, object: ObjectId) -> Option<RuntimeEntityId>;
    fn object_of(&self, id: RuntimeEntityId) -> Option<ObjectId>;
    fn is_alive(&self, id: RuntimeEntityId) -> bool {
        self.object_of(id).is_some()
    }
}

impl EntityApi for EntityRegistry {
    fn spawn_entity(&mut self, object: ObjectId) -> Result<RuntimeEntityId, EntityError> {
        self.spawn(object)
    }

    fn despawn_entity(&mut self, id: RuntimeEntityId) -> Result<ObjectId, EntityError> {
        self.despawn(id)
    }

    fn entity_of(&self, object: ObjectId) -> Option<RuntimeEntityId> {
        EntityRegistry::entity_of(self, object)
    }

    fn object_of(&self, id: RuntimeEntityId) -> Option<ObjectId> {
        EntityRegistry::object_of(self, id)
    }
}

/// Запросы к физике с масками слоёв. Не зависят от конкретной реализации (собственные коробки,
/// Avian3D или Rapier за адаптером).
pub trait PhysicsQuery: SpatialQuery {
    fn raycast_masked(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        layers: u32,
    ) -> Option<RayHit>;
    fn overlap_masked(&self, shape: &Aabb, layers: u32) -> Vec<ObjectId>;
    fn shapecast(&self, shape: &Aabb, motion: Vec3, layers: u32) -> Option<ShapeHit>;
    fn contacts(&self, shape: &Aabb, layers: u32) -> Vec<Contact>;
}

impl PhysicsQuery for BoxWorld {
    fn raycast_masked(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        layers: u32,
    ) -> Option<RayHit> {
        BoxWorld::raycast_masked(self, origin, dir, max_distance, layers)
    }

    fn overlap_masked(&self, shape: &Aabb, layers: u32) -> Vec<ObjectId> {
        BoxWorld::overlap_masked(self, shape, layers)
    }

    fn shapecast(&self, shape: &Aabb, motion: Vec3, layers: u32) -> Option<ShapeHit> {
        BoxWorld::shapecast(self, shape, motion, layers)
    }

    fn contacts(&self, shape: &Aabb, layers: u32) -> Vec<Contact> {
        BoxWorld::contacts(self, shape, layers)
    }
}

/// Реестр ассетов: поиск по идентификатору прототипа и контроль лицензий.
pub trait AssetRegistry {
    fn asset(&self, id: &PrototypeId) -> Option<&AssetEntry>;
    fn assets(&self) -> Vec<&AssetEntry>;
    fn unlicensed(&self) -> Vec<PrototypeId>;
}

impl AssetRegistry for AssetManifest {
    fn asset(&self, id: &PrototypeId) -> Option<&AssetEntry> {
        self.get(id)
    }

    fn assets(&self) -> Vec<&AssetEntry> {
        self.entries().collect()
    }

    fn unlicensed(&self) -> Vec<PrototypeId> {
        AssetManifest::unlicensed(self)
    }
}

/// Запросы топологии для симуляций (атмосфера, электрика, навигация строят свои графы сами;
/// единой физической модели движок не навязывает).
pub trait SimulationTopology {
    fn room_at(&self, cell: astrocore_math::GridCell) -> Option<RoomId>;
    /// Комнаты, достижимые из `from` по каналу при текущем состоянии проёмов.
    fn reachable_rooms(
        &self,
        from: RoomId,
        channel: Channel,
        is_open: &dyn Fn(ObjectId) -> bool,
    ) -> BTreeSet<RoomId>;
    /// Что изменилось по сравнению с прежней топологией (для инкрементального обновления).
    fn changes_from(&self, previous: &Self) -> Vec<TopologyChange>
    where
        Self: Sized;
}

impl SimulationTopology for Topology {
    fn room_at(&self, cell: astrocore_math::GridCell) -> Option<RoomId> {
        Topology::room_at(self, cell)
    }

    fn reachable_rooms(
        &self,
        from: RoomId,
        channel: Channel,
        is_open: &dyn Fn(ObjectId) -> bool,
    ) -> BTreeSet<RoomId> {
        self.reachable(from, channel, is_open)
    }

    fn changes_from(&self, previous: &Self) -> Vec<TopologyChange> {
        astrocore_spatial::diff(previous, self)
    }
}

/// Действие расширения редактора: подпись и команды мира, которые оно выполнит одной операцией.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionAction {
    pub title: String,
    pub commands: Vec<WorldCommand>,
}

/// Расширение редактора: дополнительные строки в инспекторе и действия над выбором.
/// Расширение не получает записи в мир напрямую — только возвращает команды, которые редактор
/// применяет с той же валидацией и историей отмены, что и собственные инструменты.
pub trait EditorExtension: Send {
    fn id(&self) -> &str;

    /// Строки (название, значение) для выбранного объекта.
    fn inspector_rows(&self, _world: &World, _object: ObjectId) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Доступные действия для текущего выбора.
    fn actions(&self, _world: &World, _selection: &BTreeSet<ObjectId>) -> Vec<ExtensionAction> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
