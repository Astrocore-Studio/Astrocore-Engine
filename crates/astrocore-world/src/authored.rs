use std::collections::BTreeMap;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::{FloorId, WorldError};

/// Каналы проходимости портала (битовая маска): проход, обзор, звук, газ, тепло.
pub const CHANNEL_PASS: u8 = 1;
pub const CHANNEL_SIGHT: u8 = 1 << 1;
pub const CHANNEL_ACOUSTIC: u8 = 1 << 2;
pub const CHANNEL_GAS: u8 = 1 << 3;
pub const CHANNEL_HEAT: u8 = 1 << 4;
pub const CHANNELS_ALL: u8 = 0x1F;

pub const MAX_METADATA_ENTRIES: usize = 32;
pub const MAX_METADATA_KEY: usize = 64;
pub const MAX_METADATA_VALUE: usize = 256;
/// Предел координат авторских объектов, мм (100 км).
pub const MAX_AUTHORED_MM: i32 = 100_000_000;

/// Вид авторского объекта — то, что нельзя вывести из структурной геометрии (раздел 8, 10):
/// размеченные области, явные объёмы и порталы для свободной геометрии, именованные точки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthoredKind {
    /// Логическая область (не обязательно замкнутая): для правил игры и симуляций.
    Zone,
    /// Явный объём пространства (например, внутри свободной модели) для топологии.
    Volume,
    /// Соединение этого объёма с другим по независимым каналам.
    Portal { to: ObjectId, channels: u8 },
    /// Именованная точка размещения сущности (появление, маркер); прототип задаёт потребитель.
    Placement { prototype: PrototypeId },
}

/// Авторский объект мира: границы, поворот и произвольные метаданные.
/// Целочисленные миллиметры — как у свободных моделей, для сравнения и хеширования.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoredObject {
    pub id: ObjectId,
    pub kind: AuthoredKind,
    pub floor: FloorId,
    pub min_mm: [i32; 3],
    pub max_mm: [i32; 3],
    /// Поворот вокруг +Y, сотые доли градуса.
    pub yaw_cdeg: i32,
    pub metadata: BTreeMap<String, String>,
}

impl AuthoredObject {
    pub fn new(id: ObjectId, kind: AuthoredKind, floor: FloorId, min: Vec3, max: Vec3) -> Self {
        let mm = |v: f32| (v * 1000.0).round() as i32;
        let (lo, hi) = (min.min(max), min.max(max));
        Self {
            id,
            kind,
            floor,
            min_mm: [mm(lo.x), mm(lo.y), mm(lo.z)],
            max_mm: [mm(hi.x), mm(hi.y), mm(hi.z)],
            yaw_cdeg: 0,
            metadata: BTreeMap::new(),
        }
    }

    pub fn min(&self) -> Vec3 {
        Vec3::new(
            self.min_mm[0] as f32 / 1000.0,
            self.min_mm[1] as f32 / 1000.0,
            self.min_mm[2] as f32 / 1000.0,
        )
    }

    pub fn max(&self) -> Vec3 {
        Vec3::new(
            self.max_mm[0] as f32 / 1000.0,
            self.max_mm[1] as f32 / 1000.0,
            self.max_mm[2] as f32 / 1000.0,
        )
    }

    pub fn contains(&self, p: Vec3) -> bool {
        let (lo, hi) = (self.min(), self.max());
        p.cmpge(lo).all() && p.cmple(hi).all()
    }

    pub fn is_volume_like(&self) -> bool {
        matches!(self.kind, AuthoredKind::Volume | AuthoredKind::Zone)
    }

    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        let bad = |m: &str| Err(WorldError::InvalidAuthored(format!("{}: {m}", self.id)));
        for axis in 0..3 {
            let (lo, hi) = (self.min_mm[axis], self.max_mm[axis]);
            if lo.abs() > MAX_AUTHORED_MM || hi.abs() > MAX_AUTHORED_MM {
                return bad("координата вне допустимого диапазона");
            }
            if lo > hi {
                return bad("минимум больше максимума");
            }
            let flat_ok = matches!(self.kind, AuthoredKind::Placement { .. });
            if lo == hi && !flat_ok {
                return bad("объём нулевого размера");
            }
        }
        if let AuthoredKind::Portal { to, channels } = &self.kind {
            if *to == self.id {
                return bad("портал ведёт сам в себя");
            }
            if *channels == 0 || *channels & !CHANNELS_ALL != 0 {
                return bad("неверная маска каналов");
            }
        }
        if self.metadata.len() > MAX_METADATA_ENTRIES {
            return bad("слишком много метаданных");
        }
        for (k, v) in &self.metadata {
            if k.is_empty() || k.len() > MAX_METADATA_KEY || v.len() > MAX_METADATA_VALUE {
                return bad("ключ или значение метаданных вне лимита");
            }
        }
        Ok(())
    }
}
