use astrocore_math::Aabb;
use astrocore_math::glam::{Mat4, Vec3};

use crate::MeshData;

pub const MAX_GLB_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_VERTICES: usize = 1_000_000;
pub const MAX_INDICES: usize = 3_000_000;
pub const MAX_NODE_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum GlbError {
    #[error("файл слишком велик ({0} байт)")]
    TooLarge(usize),
    #[error("не GLB или повреждён: {0}")]
    Parse(String),
    #[error("поддерживаются только буферы, встроенные в GLB (внешние ссылки запрещены)")]
    ExternalBuffer,
    #[error("в модели нет треугольных мешей")]
    Empty,
    #[error("слишком много вершин или индексов ({vertices} / {indices})")]
    TooComplex { vertices: usize, indices: usize },
    #[error("индекс {index} выходит за пределы {vertices} вершин")]
    BadIndex { index: u32, vertices: usize },
    #[error("слишком глубокая вложенность узлов")]
    TooDeep,
    #[error("в мешах есть нечисловые координаты")]
    NotFinite,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModel {
    /// Примитивы модели; узлы сцены уже применены к вершинам.
    pub meshes: Vec<MeshData>,
    pub bounds: Aabb,
}

impl ImportedModel {
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(MeshData::triangle_count).sum()
    }
}

/// Импорт GLB. Парсер glTF может паниковать на искажённых документах (например, при
/// ссылке на несуществующий аксессор), поэтому разбор изолирован: недоверенный файл
/// никогда не должен ронять сервер или редактор.
pub fn import_glb(bytes: &[u8]) -> Result<ImportedModel, GlbError> {
    let _span = tracing::debug_span!("asset_import", bytes = bytes.len()).entered();
    if bytes.len() > MAX_GLB_BYTES {
        return Err(GlbError::TooLarge(bytes.len()));
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| import_glb_unchecked(bytes)))
        .unwrap_or_else(|_| {
            Err(GlbError::Parse(
                "внутренняя ошибка разбора glTF (документ повреждён)".into(),
            ))
        })
}

fn import_glb_unchecked(bytes: &[u8]) -> Result<ImportedModel, GlbError> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(|e| GlbError::Parse(e.to_string()))?;
    let blob = gltf.blob.as_deref();
    let mut buffers: Vec<&[u8]> = Vec::new();
    for b in gltf.buffers() {
        match b.source() {
            gltf::buffer::Source::Bin => buffers.push(blob.ok_or(GlbError::ExternalBuffer)?),
            gltf::buffer::Source::Uri(_) => return Err(GlbError::ExternalBuffer),
        }
    }

    let scene = gltf
        .default_scene()
        .or_else(|| gltf.scenes().next())
        .ok_or(GlbError::Empty)?;
    let mut meshes = Vec::new();
    let (mut vertices, mut indices) = (0usize, 0usize);
    for node in scene.nodes() {
        walk(
            &node,
            Mat4::IDENTITY,
            0,
            &buffers,
            &mut meshes,
            &mut vertices,
            &mut indices,
        )?;
    }
    if meshes.is_empty() {
        return Err(GlbError::Empty);
    }
    let mut bounds: Option<Aabb> = None;
    for m in &meshes {
        if let Some(b) = m.bounds() {
            bounds = Some(bounds.map_or(b, |a| a.union(&b)));
        }
    }
    Ok(ImportedModel {
        meshes,
        bounds: bounds.ok_or(GlbError::Empty)?,
    })
}

fn walk(
    node: &gltf::Node<'_>,
    parent: Mat4,
    depth: usize,
    buffers: &[&[u8]],
    out: &mut Vec<MeshData>,
    vertices: &mut usize,
    indices: &mut usize,
) -> Result<(), GlbError> {
    if depth > MAX_NODE_DEPTH {
        return Err(GlbError::TooDeep);
    }
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = prim.reader(|b| buffers.get(b.index()).copied());
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            *vertices += positions.len();
            let idx: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            *indices += idx.len();
            if *vertices > MAX_VERTICES || *indices > MAX_INDICES {
                return Err(GlbError::TooComplex {
                    vertices: *vertices,
                    indices: *indices,
                });
            }
            if let Some(bad) = idx.iter().copied().find(|&i| i as usize >= positions.len()) {
                return Err(GlbError::BadIndex {
                    index: bad,
                    vertices: positions.len(),
                });
            }
            if !idx.len().is_multiple_of(3) {
                return Err(GlbError::Parse("число индексов не кратно трём".into()));
            }
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|n| n.collect())
                .filter(|n: &Vec<[f32; 3]>| n.len() == positions.len())
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .filter(|t: &Vec<[f32; 2]>| t.len() == positions.len())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            let normal_matrix = world.inverse().transpose();
            let mut mesh = MeshData::default();
            for ((p, n), uv) in positions.iter().zip(&normals).zip(&uvs) {
                let tp = world.transform_point3(Vec3::from_array(*p));
                let tn = normal_matrix
                    .transform_vector3(Vec3::from_array(*n))
                    .normalize_or_zero();
                if !tp.is_finite() {
                    return Err(GlbError::NotFinite);
                }
                mesh.positions.push(tp.to_array());
                mesh.normals.push(tn.to_array());
                mesh.uvs.push(*uv);
            }
            mesh.indices = idx;
            out.push(mesh);
        }
    }
    for child in node.children() {
        walk(&child, world, depth + 1, buffers, out, vertices, indices)?;
    }
    Ok(())
}

/// Собрать GLB вручную: заданные позиции и индексы u16, узел с трансляцией (для тестов).
#[doc(hidden)]
pub fn make_test_glb(positions: &[[f32; 3]], indices: &[u16], translation: [f32; 3]) -> Vec<u8> {
    let mut bin = Vec::new();
    for p in positions {
        for c in p {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let pos_len = bin.len();
    for i in indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let (min, max) =
        positions
            .iter()
            .fold(([f32::MAX; 3], [f32::MIN; 3]), |(mut lo, mut hi), p| {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
                (lo, hi)
            });
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0,"translation":[{},{},{}]}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}],
        "accessors":[
          {{"bufferView":0,"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}},
          {{"bufferView":1,"componentType":5123,"count":{},"type":"SCALAR"}}],
        "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}},{{"buffer":0,"byteOffset":{},"byteLength":{}}}],
        "buffers":[{{"byteLength":{}}}]}}"#,
        translation[0],
        translation[1],
        translation[2],
        positions.len(),
        min[0],
        min[1],
        min[2],
        max[0],
        max[1],
        max[2],
        indices.len(),
        pos_len,
        pos_len,
        indices.len() * 2,
        bin.len()
    );
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRI: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.0]];

    #[test]
    fn imports_a_triangle_and_applies_node_transform() {
        let m = import_glb(&make_test_glb(&TRI, &[0, 1, 2], [5.0, 0.0, -3.0])).unwrap();
        assert_eq!(m.triangle_count(), 1);
        assert_eq!(
            m.meshes[0].positions[1],
            [6.0, 0.0, -3.0],
            "трансляция узла применена"
        );
        assert_eq!(m.bounds.min, Vec3::new(5.0, 0.0, -3.0));
        assert_eq!(m.bounds.max, Vec3::new(6.0, 2.0, -3.0));
        // Нормали заполнены (по умолчанию «вверх»), UV — нулями.
        assert_eq!(m.meshes[0].normals.len(), 3);
        assert_eq!(m.meshes[0].uvs.len(), 3);
    }

    #[test]
    fn hostile_files_are_rejected_without_panic() {
        let good = make_test_glb(&TRI, &[0, 1, 2], [0.0; 3]);
        assert!(matches!(import_glb(b"not a glb"), Err(GlbError::Parse(_))));
        assert!(matches!(import_glb(&[]), Err(GlbError::Parse(_))));
        assert!(matches!(
            import_glb(&vec![0u8; MAX_GLB_BYTES + 1]),
            Err(GlbError::TooLarge(_))
        ));
        // Индекс за пределами вершин.
        assert!(matches!(
            import_glb(&make_test_glb(&TRI, &[0, 1, 7], [0.0; 3])),
            Err(GlbError::BadIndex { index: 7, .. })
        ));
        // Число индексов не кратно трём.
        assert!(matches!(
            import_glb(&make_test_glb(&TRI, &[0, 1], [0.0; 3])),
            Err(GlbError::Parse(_))
        ));
        // Усечённый файл.
        assert!(import_glb(&good[..good.len() / 2]).is_err());
        // Внешний буфер.
        let text = String::from_utf8_lossy(&good).replace(
            "\"buffers\":[{\"byteLength\"",
            "\"buffers\":[{\"uri\":\"x.bin\",\"byteLength\"",
        );
        assert!(import_glb(text.as_bytes()).is_err());
        // Случайные искажения не приводят к панике.
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut bad = good.clone();
            let i = (x as usize) % bad.len();
            bad[i] = (x >> 16) as u8;
            let _ = import_glb(&bad);
        }
    }
}
