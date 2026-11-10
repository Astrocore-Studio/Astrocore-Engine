use std::collections::BTreeMap;

use astrocore_core::{Revision, SchemaVersion};
use astrocore_math::ChunkCoord;
use serde::{Deserialize, Serialize};

use crate::{FloorDef, StructureElement, World, WorldConfig, WorldError};

/// Версия схемы сериализованного состояния мира.
pub const WORLD_SCHEMA_VERSION: SchemaVersion = SchemaVersion(1);

/// Сериализуемое представление мира. Содержит только каноническую часть:
/// производные индексы и dirty-флаги при загрузке перестраиваются.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    pub schema_version: SchemaVersion,
    pub id: String,
    pub config: WorldConfig,
    pub floors: Vec<FloorDef>,
    pub elements: Vec<StructureElement>,
    pub revision: Revision,
    pub chunk_revisions: Vec<(ChunkCoord, Revision)>,
}

impl World {
    /// Снимок канонического состояния в детерминированном порядке.
    pub fn to_state(&self) -> WorldState {
        WorldState {
            schema_version: WORLD_SCHEMA_VERSION,
            id: self.id.clone(),
            config: self.config,
            floors: self.floors.values().cloned().collect(),
            elements: self.elements.values().cloned().collect(),
            revision: self.revision,
            chunk_revisions: self.chunk_revisions.iter().map(|(c, r)| (*c, *r)).collect(),
        }
    }

    /// Восстановление мира с полной повторной валидацией (данные с диска не доверенные).
    pub fn from_state(state: WorldState) -> Result<World, WorldError> {
        if state.schema_version != WORLD_SCHEMA_VERSION {
            return Err(WorldError::UnsupportedSchema {
                found: state.schema_version.0,
                supported: WORLD_SCHEMA_VERSION.0,
            });
        }
        let mut world = World::new(state.id, state.config)?;
        for def in state.floors {
            def.validate()?;
            if world.floors.insert(def.id, def.clone()).is_some() {
                return Err(WorldError::DuplicateFloor(def.id));
            }
        }
        for element in state.elements {
            world.validate_element(&element)?;
            let slot = element.slot();
            if let Some(&by) = world.slots.get(&slot) {
                return Err(WorldError::SlotOccupied { slot, by });
            }
            world.index_insert(&element);
            world.elements.insert(element.id, element);
        }
        world.revision = state.revision;
        let mut chunk_revisions = BTreeMap::new();
        for (chunk, rev) in state.chunk_revisions {
            if rev > state.revision {
                return Err(WorldError::CorruptState(format!(
                    "ревизия чанка {chunk:?} ({rev:?}) больше ревизии мира"
                )));
            }
            chunk_revisions.insert(chunk, rev);
        }
        world.chunk_revisions = chunk_revisions;
        Ok(world)
    }

    /// Хеш канонического содержимого (FNV-1a 64 по каноническому JSON).
    /// Нужен для сравнения «одинаковы ли два мира» и для проверки совместимости
    /// клиента и сервера по версии карты.
    pub fn content_hash(&self) -> u64 {
        let bytes = serde_json::to_vec(&self.to_state()).unwrap_or_default();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    }
}
