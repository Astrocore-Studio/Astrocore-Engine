//! Преобразование геометрии мира в меши Bevy и цвета материалов-заглушек.

use astrocore_core::PrototypeId;
use astrocore_geometry::MeshData;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

pub fn to_bevy_mesh(data: &MeshData) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, data.uvs.clone());
    mesh.insert_indices(Indices::U32(data.indices.clone()));
    mesh
}

/// Детерминированный цвет-заглушка по идентификатору материала. Настоящие материалы
/// (текстуры, атласы) подключает игровой билд через манифест ассетов.
pub fn material_color(id: &PrototypeId) -> Color {
    let mut h: u32 = 0x811c_9dc5;
    for b in id.as_str().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let hue = (h % 360) as f32;
    Color::hsl(hue, 0.35, 0.62)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_deterministic_and_differ_between_materials() {
        let a = PrototypeId::new("t:stone").unwrap();
        let b = PrototypeId::new("t:wood").unwrap();
        assert_eq!(material_color(&a), material_color(&a));
        assert_ne!(material_color(&a), material_color(&b));
    }

    #[test]
    fn mesh_conversion_keeps_vertex_and_index_counts() {
        let data = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: vec![0, 1, 2],
        };
        let mesh = to_bevy_mesh(&data);
        assert_eq!(mesh.count_vertices(), 3);
        assert_eq!(mesh.indices().map(|i| i.len()), Some(3));
    }
}
