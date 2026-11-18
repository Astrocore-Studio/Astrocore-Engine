//! Импорт MagicaVoxel `.vox` (первая модель: чанки `SIZE`, `XYZI`, `RGBA`).
//!
//! Оси MagicaVoxel — Z вверх; переводятся поворотом `(x, y, z) → (x, z, size_y − 1 − y)` в оси
//! движка (Y вверх, правосторонняя система). Без `RGBA` берётся серая шкала.

use crate::{MAX_INPUT_BYTES, Rgba, VoxelError, VoxelModel};

fn u32_at(d: &[u8], at: usize) -> Result<u32, VoxelError> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| VoxelError::Format("файл .vox обрывается".into()))
}

/// Прочитать `.vox`. Модель получает масштаб по умолчанию и опору в центре основания.
pub fn import_vox(data: &[u8]) -> Result<VoxelModel, VoxelError> {
    if data.len() > MAX_INPUT_BYTES {
        return Err(VoxelError::TooLarge);
    }
    if data.get(0..4) != Some(b"VOX ") {
        return Err(VoxelError::Format("нет сигнатуры VOX".into()));
    }
    let mut size: Option<[u32; 3]> = None;
    let mut points: Option<Vec<[u8; 4]>> = None;
    let mut palette: Option<Vec<Rgba>> = None;
    // Чанки идут подряд за заголовком MAIN (id, длина содержимого, длина детей).
    let mut at = 8 + 12;
    while at + 12 <= data.len() {
        let id = &data[at..at + 4];
        let content = u32_at(data, at + 4)? as usize;
        let children = u32_at(data, at + 8)? as usize;
        let body = at + 12;
        let end = body
            .checked_add(content)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| VoxelError::Format("чанк выходит за файл".into()))?;
        match id {
            b"SIZE" if size.is_none() => {
                size = Some([
                    u32_at(data, body)?,
                    u32_at(data, body + 4)?,
                    u32_at(data, body + 8)?,
                ]);
            }
            b"XYZI" if points.is_none() => {
                let n = u32_at(data, body)? as usize;
                let list = n
                    .checked_mul(4)
                    .and_then(|len| (body + 4).checked_add(len))
                    .and_then(|stop| data.get(body + 4..stop))
                    .ok_or_else(|| VoxelError::Format("XYZI короче заявленного".into()))?;
                points = Some(list.as_chunks::<4>().0.to_vec());
            }
            b"RGBA" => {
                let list = data
                    .get(body..body + 256 * 4)
                    .ok_or_else(|| VoxelError::Format("RGBA короче 1024 байт".into()))?;
                palette = Some(list.as_chunks::<4>().0.to_vec());
            }
            _ => {}
        }
        at = end
            .checked_add(children)
            .ok_or_else(|| VoxelError::Format("чанк выходит за файл".into()))?;
    }
    let size = size.ok_or_else(|| VoxelError::Format("нет чанка SIZE".into()))?;
    let points = points.ok_or_else(|| VoxelError::Format("нет чанка XYZI".into()))?;
    let side = |v: u32| {
        u16::try_from(v)
            .ok()
            .filter(|s| (1..=crate::MAX_SIDE).contains(s))
    };
    let (Some(sx), Some(sy), Some(sz)) = (side(size[0]), side(size[1]), side(size[2])) else {
        return Err(VoxelError::BadSize([
            size[0].min(u32::from(u16::MAX)) as u16,
            size[1].min(u32::from(u16::MAX)) as u16,
            size[2].min(u32::from(u16::MAX)) as u16,
        ]));
    };
    // В MagicaVoxel цвет i хранится в palette[i - 1]; индекс 0 не используется.
    let palette: Vec<Rgba> = palette.map_or_else(
        || {
            (1..=255u16)
                .map(|i| [i as u8, i as u8, i as u8, 255])
                .collect()
        },
        |p| p.into_iter().take(255).collect(),
    );
    // Наши оси: X, Y = Z MagicaVoxel, Z = size_y − 1 − Y MagicaVoxel.
    let mut model = VoxelModel::new([sx, sz, sy], palette)?;
    for [x, y, z, c] in points {
        let (x, y, z) = (u16::from(x), u16::from(y), u16::from(z));
        if x >= sx || y >= sy || z >= sz || c == 0 {
            continue;
        }
        model.set(x, z, sy - 1 - y, c)?;
    }
    Ok(model)
}
