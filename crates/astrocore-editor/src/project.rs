use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;

use astrocore_core::ObjectId;
use astrocore_prefabs::{PrefabInstance, PrefabLibraryFile, PrefabRegistry};
use astrocore_world::{World, WorldState};
use serde::{Deserialize, Serialize};

pub const PROJECT_FORMAT_VERSION: u32 = 1;
const MAX_PROJECT_BYTES: u64 = 256 * 1024 * 1024;

/// Экземпляр префаба и объекты, которые он порождает в мире.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub instance: PrefabInstance,
    pub spawned: BTreeSet<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub format_version: u32,
    pub name: String,
    pub world: WorldState,
    pub prefabs: PrefabLibraryFile,
    pub instances: Vec<InstanceRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("версия проекта {0} не поддерживается (текущая {PROJECT_FORMAT_VERSION})")]
    Version(u32),
    #[error("проект слишком велик ({0} байт)")]
    TooLarge(u64),
    #[error("мир проекта некорректен: {0}")]
    World(String),
    #[error("префабы проекта некорректны: {0}")]
    Prefabs(String),
    #[error("экземпляр ссылается на неизвестный префаб {0}")]
    UnknownPrefab(String),
}

pub struct Project {
    pub name: String,
    pub world: World,
    pub prefabs: PrefabRegistry,
    pub instances: Vec<InstanceRecord>,
}

impl Project {
    pub fn new(name: impl Into<String>, world: World) -> Self {
        Self {
            name: name.into(),
            world,
            prefabs: PrefabRegistry::new(),
            instances: Vec::new(),
        }
    }

    pub fn to_file(&self) -> ProjectFile {
        ProjectFile {
            format_version: PROJECT_FORMAT_VERSION,
            name: self.name.clone(),
            world: self.world.to_state(),
            prefabs: self.prefabs.to_file(),
            instances: self.instances.clone(),
        }
    }

    /// Атомарная запись: во временный файл, затем переименование.
    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let bytes = serde_json::to_vec_pretty(&self.to_file())?;
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Загрузка недоверенного файла с полной проверкой.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let len = fs::metadata(path)?.len();
        if len > MAX_PROJECT_BYTES {
            return Err(ProjectError::TooLarge(len));
        }
        let file: ProjectFile = serde_json::from_slice(&fs::read(path)?)?;
        Self::from_file(file)
    }

    pub fn from_file(file: ProjectFile) -> Result<Self, ProjectError> {
        if file.format_version != PROJECT_FORMAT_VERSION {
            return Err(ProjectError::Version(file.format_version));
        }
        let world =
            World::from_state(file.world).map_err(|e| ProjectError::World(e.to_string()))?;
        let json = serde_json::to_string(&file.prefabs)?;
        let prefabs =
            PrefabRegistry::from_json(&json).map_err(|e| ProjectError::Prefabs(e.to_string()))?;
        for rec in &file.instances {
            if prefabs.get(&rec.instance.prefab).is_none() {
                return Err(ProjectError::UnknownPrefab(rec.instance.prefab.to_string()));
            }
        }
        Ok(Self {
            name: file.name,
            world,
            prefabs,
            instances: file.instances,
        })
    }
}
