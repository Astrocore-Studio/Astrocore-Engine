//! Сборка меша: по каждому из шести направлений грани одного цвета, смотрящие в пустоту,
//! объединяются в прямоугольники (жадно); внутренние грани не строятся.

use crate::VoxelModel;

/// Меш модели: позиции в метрах относительно опоры, нормали, цвета вершин (sRGB, 0…1), индексы
/// треугольников (против часовой стрелки снаружи).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VoxelMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl VoxelMesh {
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Собрать меш модели.
pub fn build_mesh(model: &VoxelModel) -> VoxelMesh {
    let size = model.size().map(i32::from);
    let scale = 1.0 / f32::from(model.voxels_per_meter());
    let pivot = model.pivot();
    let mut mesh = VoxelMesh::default();
    for axis in 0..3usize {
        // Оси грани: u = следующая по кругу, v = через одну; e_u × e_v = e_axis.
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        for sign in [1i32, -1] {
            let mut normal = [0.0f32; 3];
            normal[axis] = sign as f32;
            for slice in 0..size[axis] {
                // Маска слоя: цвет грани или 0.
                let (w, h) = (size[u] as usize, size[v] as usize);
                let mut mask = vec![0u8; w * h];
                for j in 0..size[v] {
                    for i in 0..size[u] {
                        let mut p = [0i32; 3];
                        p[axis] = slice;
                        p[u] = i;
                        p[v] = j;
                        let c = model.get(p[0], p[1], p[2]);
                        if c == 0 {
                            continue;
                        }
                        let mut q = p;
                        q[axis] += sign;
                        if model.get(q[0], q[1], q[2]) == 0 {
                            mask[i as usize + j as usize * w] = c;
                        }
                    }
                }
                // Жадное объединение: расширяем по u, затем по v, пока цвет совпадает.
                for j in 0..h {
                    let mut i = 0;
                    while i < w {
                        let c = mask[i + j * w];
                        if c == 0 {
                            i += 1;
                            continue;
                        }
                        let mut width = 1;
                        while i + width < w && mask[i + width + j * w] == c {
                            width += 1;
                        }
                        let mut height = 1;
                        'grow: while j + height < h {
                            for k in 0..width {
                                if mask[i + k + (j + height) * w] != c {
                                    break 'grow;
                                }
                            }
                            height += 1;
                        }
                        for dj in 0..height {
                            for k in 0..width {
                                mask[i + k + (j + dj) * w] = 0;
                            }
                        }
                        let rgba = model.palette()[usize::from(c) - 1];
                        let color = rgba.map(|b| f32::from(b) / 255.0);
                        let plane = (slice + i32::from(sign > 0)) as f32;
                        let corner = |du: usize, dv: usize| {
                            let mut p = [0.0f32; 3];
                            p[axis] = plane;
                            p[u] = (i + du) as f32;
                            p[v] = (j + dv) as f32;
                            [
                                (p[0] - pivot[0]) * scale,
                                (p[1] - pivot[1]) * scale,
                                (p[2] - pivot[2]) * scale,
                            ]
                        };
                        let mut quad = [
                            corner(0, 0),
                            corner(width, 0),
                            corner(width, height),
                            corner(0, height),
                        ];
                        if sign < 0 {
                            quad.reverse();
                        }
                        let base = mesh.positions.len() as u32;
                        mesh.positions.extend(quad);
                        mesh.normals.extend([normal; 4]);
                        mesh.colors.extend([color; 4]);
                        mesh.indices
                            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
                        i += width;
                    }
                }
            }
        }
    }
    mesh
}
