//! Ресурсы игры: каталоги контента в порядке приоритета. Образец —
//! `Robust.Shared/ContentPack/ResourceManager.cs` @ 1afd692.
//!
//! Раскладка каталога — как у `Resources/` SS14: `Prototypes/`, `Textures/`, `Audio/`,
//! `Maps/`, `Locale/`. Каталогов может быть несколько: свой контент игры поверх контента SS14.
//! Файл ищется во всех каталогах по порядку подключения; первый найденный выигрывает.

mod rsi;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use astrocore_data::{DataFormat, DataNode};
use astrocore_ecs::ComponentRegistry;
use astrocore_prototypes::{LoadReport, Prototypes};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use rayon::prelude::*;

pub use rsi::{RsiDirections, RsiError, RsiMeta, RsiState};

#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    #[error("недопустимый путь ресурса {0:?}")]
    BadPath(String),
    #[error("ресурс {0} не найден")]
    NotFound(String),
    #[error("ресурс {0}: {1}")]
    Io(String, std::io::Error),
}

/// Приводит путь ресурса к виду `Textures/Objects/crowbar.rsi/icon.png`: разделители `/`,
/// без ведущей косой черты. Пути с `..` и абсолютные пути Windows отклоняются.
pub fn normalize_path(path: &str) -> Result<String, ResourceError> {
    let unified = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(ResourceError::BadPath(path.to_owned())),
            part if part.contains(':') => return Err(ResourceError::BadPath(path.to_owned())),
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

/// Каталоги контента. Ресурс `World`.
#[derive(Resource, Debug, Clone, Default)]
pub struct Resources {
    roots: Vec<PathBuf>,
}

impl Resources {
    /// Подключает каталог контента. Раньше подключённый каталог важнее.
    pub fn mount(&mut self, root: impl Into<PathBuf>) {
        let root = root.into();
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }

    /// Подключённые каталоги в порядке приоритета.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Файл ресурса на диске.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, ResourceError> {
        let relative = normalize_path(path)?;
        self.roots
            .iter()
            .map(|root| root.join(&relative))
            .find(|candidate| candidate.is_file())
            .ok_or(ResourceError::NotFound(relative))
    }

    pub fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>, ResourceError> {
        let file = self.resolve(path)?;
        std::fs::read(&file).map_err(|e| ResourceError::Io(path.to_owned(), e))
    }

    pub fn read_to_string(&self, path: &str) -> Result<String, ResourceError> {
        let file = self.resolve(path)?;
        let text =
            std::fs::read_to_string(&file).map_err(|e| ResourceError::Io(path.to_owned(), e))?;
        // BOM встречается в ресурсах SS14.
        Ok(match text.strip_prefix('\u{feff}') {
            Some(stripped) => stripped.to_owned(),
            None => text,
        })
    }

    /// Все файлы каталога ресурсов и его подкаталогов во всех подключённых каталогах,
    /// без повторов, по алфавиту.
    pub fn find_files(&self, dir: &str) -> Result<Vec<String>, ResourceError> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else if let Ok(relative) = path.strip_prefix(base) {
                    out.insert(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }

        let relative = normalize_path(dir)?;
        let mut found = BTreeSet::new();
        for root in &self.roots {
            walk(root, &root.join(&relative), &mut found);
        }
        Ok(found.into_iter().collect())
    }

    /// Описание набора спрайтов: `meta.json` каталога `.rsi`.
    pub fn rsi_meta(&self, rsi: &str) -> Result<RsiMeta, RsiError> {
        let path = format!("{}/meta.json", normalize_path(rsi)?);
        RsiMeta::parse(&self.read_to_string(&path)?)
    }
}

/// Ресурсы на `World`.
pub trait ResourcesWorldExt {
    /// Загружает прототипы из каталогов `Prototypes/` всех подключённых каталогов и
    /// разрешает наследование. Файл с тем же путём в более важном каталоге заменяет файл
    /// из менее важного.
    fn load_prototypes_from_resources(&mut self) -> LoadReport;
}

impl ResourcesWorldExt for World {
    fn load_prototypes_from_resources(&mut self) -> LoadReport {
        let mut report = LoadReport::default();
        let resources = self.get_resource_or_insert_with(Resources::default).clone();
        let files: Vec<String> = resources
            .find_files("Prototypes")
            .unwrap_or_default()
            .into_iter()
            .filter(|path| DataFormat::from_path(Path::new(path)).is_some())
            .collect();
        let parsed: Vec<(String, Result<DataNode, String>)> = files
            .into_par_iter()
            .map(|path| {
                let node = resources
                    .read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        DataFormat::from_path(Path::new(&path))
                            .expect("формат проверен выше")
                            .parse(&text)
                            .map_err(|e| e.to_string())
                    });
                (path, node)
            })
            .collect();
        self.init_resource::<Prototypes>();
        self.init_resource::<ComponentRegistry>();
        self.resource_scope(|world, mut prototypes: bevy_ecs::world::Mut<Prototypes>| {
            for (path, node) in parsed {
                match node {
                    Ok(node) => prototypes.load_node(&path, &node, &mut report),
                    Err(message) => report.errors.push(astrocore_prototypes::PrototypeError {
                        source: path,
                        line: 0,
                        message,
                    }),
                }
            }
            prototypes.resolve(world.resource::<ComponentRegistry>(), &mut report);
        });
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    #[test]
    fn paths_are_normalized_and_confined() {
        assert_eq!(
            normalize_path("/Textures\\a.rsi//icon.png").unwrap(),
            "Textures/a.rsi/icon.png"
        );
        assert_eq!(normalize_path("./Audio/x.ogg").unwrap(), "Audio/x.ogg");
        assert!(normalize_path("Textures/../../etc/passwd").is_err());
        assert!(normalize_path("C:/Windows").is_err());
    }

    #[test]
    fn earlier_roots_override_later_ones() {
        let (own, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(
            base.path(),
            "Prototypes/items.yml",
            "- type: entity\n  id: Crowbar\n  name: base\n",
        );
        write(
            base.path(),
            "Prototypes/walls.yml",
            "- type: entity\n  id: Wall\n",
        );
        write(
            own.path(),
            "Prototypes/items.yml",
            "- type: entity\n  id: Crowbar\n  name: own\n",
        );
        write(
            own.path(),
            "Prototypes/Own/lamp.ron",
            "[entity(id: \"Lamp\")]",
        );
        write(base.path(), "Prototypes/readme.txt", "не прототип");
        write(own.path(), "Textures/a.png", "\u{feff}png");

        let mut resources = Resources::default();
        resources.mount(own.path());
        resources.mount(base.path());
        resources.mount(own.path());
        assert_eq!(resources.roots().len(), 2);
        assert_eq!(
            resources.find_files("Prototypes").unwrap(),
            [
                "Prototypes/Own/lamp.ron",
                "Prototypes/items.yml",
                "Prototypes/readme.txt",
                "Prototypes/walls.yml"
            ]
        );
        assert_eq!(resources.read_to_string("/Textures/a.png").unwrap(), "png");
        assert!(matches!(
            resources.read("Textures/missing.png"),
            Err(ResourceError::NotFound(_))
        ));
        assert!(resources.exists("Prototypes/walls.yml"));

        let mut world = World::new();
        world.insert_resource(resources);
        let report = world.load_prototypes_from_resources();
        assert_eq!(report.errors, []);
        assert_eq!(report.files, 3);
        let prototypes = world.resource::<Prototypes>();
        assert_eq!(
            prototypes.entity("Crowbar").unwrap().name.as_deref(),
            Some("own")
        );
        assert!(prototypes.entity("Wall").is_some() && prototypes.entity("Lamp").is_some());
    }
}
