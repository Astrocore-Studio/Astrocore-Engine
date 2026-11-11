use std::collections::HashMap;
use std::path::PathBuf;

use astrocore_core::PrototypeId;
use bevy::prelude::*;

use crate::to_bevy_mesh;

/// Имя файла модели по идентификатору: `ns:a/b` → `ns_a_b.glb`.
pub fn model_file_name(id: &PrototypeId) -> String {
    let flat: String = id
        .as_str()
        .chars()
        .map(|c| if c == ':' || c == '/' { '_' } else { c })
        .collect();
    format!("{flat}.glb")
}

#[derive(Resource, Default)]
pub struct ModelLibrary {
    dir: Option<PathBuf>,
    loaded: HashMap<PrototypeId, Option<Vec<Handle<Mesh>>>>,
    /// Сообщения о неудачных загрузках (по одному на модель).
    pub errors: Vec<String>,
}

impl ModelLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
            ..Self::default()
        }
    }

    pub fn is_configured(&self) -> bool {
        self.dir.is_some()
    }

    /// Меши модели (по одному на примитив) или `None`, если модели нет. Результат кэшируется.
    pub fn get(
        &mut self,
        id: &PrototypeId,
        meshes: &mut Assets<Mesh>,
    ) -> Option<Vec<Handle<Mesh>>> {
        if let Some(cached) = self.loaded.get(id) {
            return cached.clone();
        }
        let loaded = self.load(id, meshes);
        self.loaded.insert(id.clone(), loaded.clone());
        loaded
    }

    fn load(&mut self, id: &PrototypeId, meshes: &mut Assets<Mesh>) -> Option<Vec<Handle<Mesh>>> {
        let dir = self.dir.as_ref()?;
        let path = dir.join(model_file_name(id));
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.errors
                    .push(format!("модель {id}: {}: {e}", path.display()));
                return None;
            }
        };
        match astrocore_geometry::glb::import_glb(&bytes) {
            Ok(model) => Some(
                model
                    .meshes
                    .iter()
                    .map(|m| meshes.add(to_bevy_mesh(m)))
                    .collect(),
            ),
            Err(e) => {
                self.errors.push(format!("модель {id}: {e}"));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_flat_and_safe() {
        let id = PrototypeId::new("fantasy:props/statue-01").unwrap();
        assert_eq!(model_file_name(&id), "fantasy_props_statue-01.glb");
        // Идентификатор не может содержать «..» или слэши, выводящие из каталога, — имя всегда плоское.
        assert!(!model_file_name(&PrototypeId::new("a:b/c.d").unwrap()).contains('/'));
    }

    #[test]
    fn models_load_from_disk_cache_and_report_failures_once() {
        let dir = std::env::temp_dir().join(format!("acr-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tri = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.0]];
        std::fs::write(
            dir.join("t_good.glb"),
            astrocore_geometry::glb::make_test_glb(&tri, &[0, 1, 2], [0.0; 3]),
        )
        .unwrap();
        std::fs::write(dir.join("t_bad.glb"), b"not a glb").unwrap();
        let mut lib = ModelLibrary::new(&dir);
        let mut meshes = Assets::<Mesh>::default();
        let good = PrototypeId::new("t:good").unwrap();
        let h = lib.get(&good, &mut meshes).expect("модель загружена");
        assert_eq!(h.len(), 1);
        assert_eq!(meshes.get(&h[0]).unwrap().count_vertices(), 3);
        let again = lib.get(&good, &mut meshes).unwrap();
        assert_eq!(again[0], h[0], "кэш: тот же ресурс");
        for id in ["t:bad", "t:missing"] {
            assert!(
                lib.get(&PrototypeId::new(id).unwrap(), &mut meshes)
                    .is_none()
            );
            assert!(
                lib.get(&PrototypeId::new(id).unwrap(), &mut meshes)
                    .is_none()
            );
        }
        assert_eq!(
            lib.errors.len(),
            2,
            "по одному сообщению на модель: {:?}",
            lib.errors
        );
        assert!(!ModelLibrary::default().is_configured());
        assert!(ModelLibrary::default().get(&good, &mut meshes).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
