//! Бинарный формат `.avox` (ADR-0011): заголовок, палитра, воксели сериями RLE.

use crate::{MAX_INPUT_BYTES, Rgba, VoxelError, VoxelModel};

const MAGIC: &[u8; 4] = b"AVOX";
const VERSION: u16 = 1;

/// Записать модель в `.avox`.
pub fn encode(model: &VoxelModel) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    for s in model.size() {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(&model.voxels_per_meter().to_le_bytes());
    for p in model.pivot() {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out.extend_from_slice(&(model.palette().len() as u16).to_le_bytes());
    for c in model.palette() {
        out.extend_from_slice(c);
    }
    let raw = model.raw();
    let mut i = 0;
    while i < raw.len() {
        let v = raw[i];
        let mut run = 1usize;
        while i + run < raw.len() && raw[i + run] == v && run < usize::from(u16::MAX) {
            run += 1;
        }
        out.extend_from_slice(&(run as u16).to_le_bytes());
        out.push(v);
        i += run;
    }
    out
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], VoxelError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| VoxelError::Format("файл обрывается".into()))?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, VoxelError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn f32(&mut self) -> Result<f32, VoxelError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Прочитать `.avox`; недоверенные данные проверяются, паник нет.
pub fn decode(data: &[u8]) -> Result<VoxelModel, VoxelError> {
    if data.len() > MAX_INPUT_BYTES {
        return Err(VoxelError::TooLarge);
    }
    let mut r = Reader { data, at: 0 };
    if r.take(4)? != MAGIC {
        return Err(VoxelError::Format("нет сигнатуры AVOX".into()));
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(VoxelError::Format(format!(
            "версия {version} не поддерживается"
        )));
    }
    let size = [r.u16()?, r.u16()?, r.u16()?];
    if size.iter().any(|&s| s == 0 || s > crate::MAX_SIDE) {
        return Err(VoxelError::BadSize(size));
    }
    let scale = r.u16()?;
    let pivot = [r.f32()?, r.f32()?, r.f32()?];
    let colors = usize::from(r.u16()?);
    if colors == 0 || colors > 255 {
        return Err(VoxelError::BadPalette(colors));
    }
    let mut palette: Vec<Rgba> = Vec::with_capacity(colors);
    for _ in 0..colors {
        let c = r.take(4)?;
        palette.push([c[0], c[1], c[2], c[3]]);
    }
    let total: usize = size.iter().map(|&s| usize::from(s)).product();
    let mut voxels = Vec::with_capacity(total);
    while voxels.len() < total {
        let run = usize::from(r.u16()?);
        let v = r.take(1)?[0];
        if run == 0 || voxels.len() + run > total {
            return Err(VoxelError::Format("неверная длина серии".into()));
        }
        voxels.resize(voxels.len() + run, v);
    }
    if r.at != data.len() {
        return Err(VoxelError::Format("лишние данные в конце".into()));
    }
    VoxelModel::from_parts(size, scale, pivot, palette, voxels)
}
