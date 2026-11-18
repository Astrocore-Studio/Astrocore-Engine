//! Воксельные модели Astrocore (ADR-0011): плотная сетка индексов палитры, масштаб и точка опоры;
//! бинарный формат `.avox`, импорт MagicaVoxel `.vox`, сборка меша с объединением граней.
//!
//! Оси — как в движке: Y вверх, правосторонняя система, перед модели смотрит в +Z.

mod codec;
mod mesh;
mod vox;

pub use codec::{decode, encode};
pub use mesh::{VoxelMesh, build_mesh};
pub use vox::import_vox;

/// Наибольшая сторона модели, вокселей.
pub const MAX_SIDE: u16 = 256;
/// Предел размера входного файла, байт.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Вокселей на метр по умолчанию: один воксель — один пиксель спрайтов (32 на метр).
pub const DEFAULT_VOXELS_PER_METER: u16 = 32;

/// Цвет палитры: RGBA, sRGB.
pub type Rgba = [u8; 4];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoxelError {
    #[error("размер модели {0:?}: каждая сторона 1…{MAX_SIDE}")]
    BadSize([u16; 3]),
    #[error("палитра: {0} цветов, нужно 1…255")]
    BadPalette(usize),
    #[error("воксель ссылается на цвет {0}, а в палитре {1}")]
    BadIndex(u8, usize),
    #[error("масштаб {0} вокселей на метр: нужно 1…1024")]
    BadScale(u16),
    #[error("файл больше {MAX_INPUT_BYTES} байт")]
    TooLarge,
    #[error("неверный формат: {0}")]
    Format(String),
}

/// Воксельная модель: `size` по X, Y, Z; `voxels` — индексы палитры порядком X → Y → Z
/// (`0` — пусто, `i` — цвет `palette[i - 1]`).
#[derive(Debug, Clone, PartialEq)]
pub struct VoxelModel {
    size: [u16; 3],
    voxels_per_meter: u16,
    pivot: [f32; 3],
    palette: Vec<Rgba>,
    voxels: Vec<u8>,
}

impl VoxelModel {
    /// Пустая модель с палитрой; опора — центр основания.
    pub fn new(size: [u16; 3], palette: Vec<Rgba>) -> Result<Self, VoxelError> {
        if size.iter().any(|&s| s == 0 || s > MAX_SIDE) {
            return Err(VoxelError::BadSize(size));
        }
        if palette.is_empty() || palette.len() > 255 {
            return Err(VoxelError::BadPalette(palette.len()));
        }
        let n = size.iter().map(|&s| usize::from(s)).product();
        Ok(Self {
            size,
            voxels_per_meter: DEFAULT_VOXELS_PER_METER,
            pivot: [f32::from(size[0]) / 2.0, 0.0, f32::from(size[2]) / 2.0],
            palette,
            voxels: vec![0; n],
        })
    }

    pub fn size(&self) -> [u16; 3] {
        self.size
    }

    pub fn palette(&self) -> &[Rgba] {
        &self.palette
    }

    pub fn voxels_per_meter(&self) -> u16 {
        self.voxels_per_meter
    }

    pub fn set_voxels_per_meter(&mut self, v: u16) -> Result<(), VoxelError> {
        if v == 0 || v > 1024 {
            return Err(VoxelError::BadScale(v));
        }
        self.voxels_per_meter = v;
        Ok(())
    }

    /// Точка опоры в вокселях (ставится в позицию сущности).
    pub fn pivot(&self) -> [f32; 3] {
        self.pivot
    }

    pub fn set_pivot(&mut self, pivot: [f32; 3]) {
        self.pivot = pivot;
    }

    fn index(&self, x: u16, y: u16, z: u16) -> Option<usize> {
        let [sx, sy, sz] = self.size;
        (x < sx && y < sy && z < sz).then(|| {
            usize::from(x) + usize::from(sx) * (usize::from(y) + usize::from(sy) * usize::from(z))
        })
    }

    /// Индекс палитры вокселя (`0` — пусто); за пределами модели — пусто.
    pub fn get(&self, x: i32, y: i32, z: i32) -> u8 {
        let (Ok(x), Ok(y), Ok(z)) = (u16::try_from(x), u16::try_from(y), u16::try_from(z)) else {
            return 0;
        };
        self.index(x, y, z).map_or(0, |i| self.voxels[i])
    }

    /// Поставить воксель цвета `color` (индекс палитры, `0` — стереть).
    pub fn set(&mut self, x: u16, y: u16, z: u16, color: u8) -> Result<(), VoxelError> {
        if usize::from(color) > self.palette.len() {
            return Err(VoxelError::BadIndex(color, self.palette.len()));
        }
        let i = self.index(x, y, z).ok_or(VoxelError::BadSize([x, y, z]))?;
        self.voxels[i] = color;
        Ok(())
    }

    /// Цвет вокселя, если он не пуст.
    pub fn color(&self, x: i32, y: i32, z: i32) -> Option<Rgba> {
        match self.get(x, y, z) {
            0 => None,
            i => self.palette.get(usize::from(i) - 1).copied(),
        }
    }

    /// Сколько вокселей занято.
    pub fn filled(&self) -> usize {
        self.voxels.iter().filter(|&&v| v != 0).count()
    }

    pub(crate) fn raw(&self) -> &[u8] {
        &self.voxels
    }

    pub(crate) fn from_parts(
        size: [u16; 3],
        voxels_per_meter: u16,
        pivot: [f32; 3],
        palette: Vec<Rgba>,
        voxels: Vec<u8>,
    ) -> Result<Self, VoxelError> {
        let mut m = Self::new(size, palette)?;
        m.set_voxels_per_meter(voxels_per_meter)?;
        if voxels.len() != m.voxels.len() {
            return Err(VoxelError::Format(
                "число вокселей не совпадает с размером".into(),
            ));
        }
        if let Some(&bad) = voxels.iter().find(|&&v| usize::from(v) > m.palette.len()) {
            return Err(VoxelError::BadIndex(bad, m.palette.len()));
        }
        if pivot.iter().any(|p| !p.is_finite()) {
            return Err(VoxelError::Format("опора не число".into()));
        }
        m.pivot = pivot;
        m.voxels = voxels;
        Ok(m)
    }
}

#[cfg(test)]
mod tests;
