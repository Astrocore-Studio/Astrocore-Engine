use std::collections::HashMap;

use crate::ObjectId;

/// Эфемерный дескриптор сущности: индекс и поколение. Не сериализуется и не хранится в файлах;
/// устаревший дескриптор (сущность уничтожена, индекс переиспользован) распознаётся по поколению.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeEntityId {
    index: u32,
    generation: u32,
}

impl RuntimeEntityId {
    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntityError {
    #[error("объект {0} уже имеет сущность")]
    AlreadySpawned(ObjectId),
    #[error("сущность уничтожена или дескриптор устарел")]
    Stale,
}

#[derive(Debug, Clone)]
struct Slot {
    generation: u32,
    object: Option<ObjectId>,
}

/// Постоянное соответствие «долговечный `ObjectId` ↔ эфемерная сущность». Файлы и сеть
/// ссылаются на `ObjectId`; рантайм работает с дескрипторами, защищёнными поколениями.
#[derive(Debug, Default, Clone)]
pub struct EntityRegistry {
    slots: Vec<Slot>,
    free: Vec<u32>,
    by_object: HashMap<ObjectId, RuntimeEntityId>,
}

impl EntityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.by_object.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_object.is_empty()
    }

    pub fn spawn(&mut self, object: ObjectId) -> Result<RuntimeEntityId, EntityError> {
        if self.by_object.contains_key(&object) {
            return Err(EntityError::AlreadySpawned(object));
        }
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    object: None,
                });
                (self.slots.len() - 1) as u32
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.object = Some(object);
        let id = RuntimeEntityId {
            index,
            generation: slot.generation,
        };
        self.by_object.insert(object, id);
        Ok(id)
    }

    /// Уничтожить сущность; индекс освобождается, поколение растёт, старые дескрипторы устаревают.
    pub fn despawn(&mut self, id: RuntimeEntityId) -> Result<ObjectId, EntityError> {
        let object = self.object_of(id).ok_or(EntityError::Stale)?;
        let slot = &mut self.slots[id.index as usize];
        slot.object = None;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index);
        self.by_object.remove(&object);
        Ok(object)
    }

    pub fn is_alive(&self, id: RuntimeEntityId) -> bool {
        self.object_of(id).is_some()
    }

    pub fn object_of(&self, id: RuntimeEntityId) -> Option<ObjectId> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.object)
            .flatten()
    }

    pub fn entity_of(&self, object: ObjectId) -> Option<RuntimeEntityId> {
        self.by_object.get(&object).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(n: u128) -> ObjectId {
        ObjectId::from_u128(n)
    }

    #[test]
    fn handles_map_both_ways_and_reject_duplicates() {
        let mut r = EntityRegistry::new();
        let a = r.spawn(o(1)).unwrap();
        assert_eq!(r.object_of(a), Some(o(1)));
        assert_eq!(r.entity_of(o(1)), Some(a));
        assert_eq!(r.spawn(o(1)), Err(EntityError::AlreadySpawned(o(1))));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn stale_handles_are_detected_after_index_reuse() {
        let mut r = EntityRegistry::new();
        let a = r.spawn(o(1)).unwrap();
        assert_eq!(r.despawn(a), Ok(o(1)));
        assert!(!r.is_alive(a));
        let b = r.spawn(o(2)).unwrap();
        assert_eq!(a.index(), b.index(), "индекс переиспользован");
        assert_ne!(a.generation(), b.generation());
        assert_eq!(
            r.object_of(a),
            None,
            "старый дескриптор не указывает на новую сущность"
        );
        assert_eq!(r.despawn(a), Err(EntityError::Stale));
        assert_eq!(r.object_of(b), Some(o(2)));
        // Объект можно породить заново после уничтожения.
        let _ = r.despawn(b).unwrap();
        assert!(r.spawn(o(2)).is_ok());
        assert!(r.is_empty() || r.len() == 1);
    }

    #[test]
    fn many_spawn_despawn_cycles_stay_consistent() {
        let mut r = EntityRegistry::new();
        let mut live = Vec::new();
        for i in 0..2000u128 {
            live.push(r.spawn(o(i)).unwrap());
            if i % 3 == 0 {
                let h = live.remove(live.len() / 2);
                r.despawn(h).unwrap();
            }
        }
        for h in &live {
            let obj = r.object_of(*h).unwrap();
            assert_eq!(r.entity_of(obj), Some(*h));
        }
        assert_eq!(r.len(), live.len());
    }
}
