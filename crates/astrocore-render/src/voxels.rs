//! Воксельные модели в рендере (ADR-0011): `.avox` или MagicaVoxel `.vox` → меш Bevy с цветами
//! вершин. Сетка строится один раз на модель и делится между всеми её экземплярами.

use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use astrocore_voxels::{VoxelMesh, VoxelModel, build_mesh, decode, import_vox};

use crate::manifest::ManifestError;

/// sRGB (0…1) → линейный цвет: цвета вершин Bevy линейные.
fn linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Меш Bevy из меша вокселей.
pub fn to_mesh(v: &VoxelMesh) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, v.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, v.normals.clone());
    let colors: Vec<[f32; 4]> = v
        .colors
        .iter()
        .map(|c| [linear(c[0]), linear(c[1]), linear(c[2]), c[3]])
        .collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(v.indices.clone()));
    mesh
}

/// Меш Bevy прямо из модели.
pub fn model_mesh(model: &VoxelModel) -> Mesh {
    to_mesh(&build_mesh(model))
}

/// Прочитать модель из файла: `.avox` (свой формат) или `.vox` (MagicaVoxel).
pub fn load_model(path: &Path) -> Result<VoxelModel, ManifestError> {
    let len = std::fs::metadata(path)
        .map_err(|e| ManifestError::Io(path.to_path_buf(), e))?
        .len();
    if len > astrocore_voxels::MAX_INPUT_BYTES as u64 {
        return Err(ManifestError::TooLarge(path.to_path_buf()));
    }
    let bytes = std::fs::read(path).map_err(|e| ManifestError::Io(path.to_path_buf(), e))?;
    let model = match path.extension().and_then(|e| e.to_str()) {
        Some("vox") => import_vox(&bytes),
        _ => decode(&bytes),
    };
    model.map_err(|e| ManifestError::Image(path.to_path_buf(), e.to_string()))
}

/// Общий материал воксельных моделей: цвет — из вершин, освещается сценой.
pub fn voxel_model_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        reflectance: 0.05,
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        ..default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_becomes_a_coloured_mesh_in_linear_space() {
        let mut m = VoxelModel::new([2, 1, 1], vec![[255, 128, 0, 255]]).unwrap();
        m.set(0, 0, 0, 1).unwrap();
        m.set(1, 0, 0, 1).unwrap();
        let mesh = model_mesh(&m);
        assert_eq!(
            mesh.indices().unwrap().len(),
            6 * 6,
            "два вокселя одного цвета — 6 граней"
        );
        let Some(bevy::mesh::VertexAttributeValues::Float32x4(colors)) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("нет цветов вершин");
        };
        let c = colors[0];
        assert!((c[0] - 1.0).abs() < 1e-6 && c[2].abs() < 1e-6);
        assert!(
            (c[1] - linear(128.0 / 255.0)).abs() < 1e-6,
            "sRGB → линейный"
        );
        assert!(c[1] < 0.5 * 128.0 / 255.0 + 0.05);
    }

    #[test]
    fn models_load_from_avox_and_vox_and_bad_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("astro-voxels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = VoxelModel::new([1, 2, 1], vec![[1, 2, 3, 255]]).unwrap();
        m.set(0, 1, 0, 1).unwrap();
        std::fs::write(dir.join("a.avox"), astrocore_voxels::encode(&m)).unwrap();
        std::fs::write(dir.join("bad.avox"), b"junk").unwrap();
        assert_eq!(load_model(&dir.join("a.avox")).unwrap(), m);
        assert!(load_model(&dir.join("bad.avox")).is_err());
        assert!(load_model(&dir.join("missing.avox")).is_err());
        assert!(load_model(&dir.join("bad.avox").with_extension("vox")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
