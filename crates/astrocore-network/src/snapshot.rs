use astrocore_protocol::{MAX_SNAPSHOT_PART_BYTES, MAX_SNAPSHOT_PARTS, ServerMessage};
use astrocore_world::WorldState;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("снимок слишком велик ({0} кусков)")]
    TooLarge(usize),
    #[error("кусок снимка не согласуется с предыдущими (total {got} вместо {expected})")]
    Inconsistent { expected: u16, got: u16 },
    #[error("кусок {0} пришёл повторно с другим содержимым")]
    Conflicting(u16),
    #[error("снимок повреждён: {0}")]
    Decode(String),
}

/// Разбить состояние мира на сообщения-части.
pub fn split(state: &WorldState) -> Result<Vec<ServerMessage>, SnapshotError> {
    split_bytes(state, |index, total, data| ServerMessage::SnapshotPart {
        index,
        total,
        data,
    })
}

/// Разбить состояние рельефа на сообщения-части.
pub fn split_terrain(
    state: &astrocore_terrain::TerrainState,
) -> Result<Vec<ServerMessage>, SnapshotError> {
    split_bytes(state, |index, total, data| {
        ServerMessage::TerrainSnapshotPart { index, total, data }
    })
}

fn split_bytes<T: serde::Serialize>(
    state: &T,
    make: impl Fn(u16, u16, Vec<u8>) -> ServerMessage,
) -> Result<Vec<ServerMessage>, SnapshotError> {
    let bytes = postcard::to_stdvec(state).map_err(|e| SnapshotError::Decode(e.to_string()))?;
    let chunks: Vec<&[u8]> = bytes.chunks(MAX_SNAPSHOT_PART_BYTES).collect();
    if chunks.len() > usize::from(MAX_SNAPSHOT_PARTS) {
        return Err(SnapshotError::TooLarge(chunks.len()));
    }
    let total = chunks.len() as u16;
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| make(i as u16, total, c.to_vec()))
        .collect())
}

/// Сборщик снимка из частей (части могут приходить повторно).
#[derive(Debug, Default)]
pub struct Reassembler {
    total: Option<u16>,
    parts: Vec<Option<Vec<u8>>>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить часть. `Some(state)` — снимок мира собран.
    pub fn push(
        &mut self,
        index: u16,
        total: u16,
        data: Vec<u8>,
    ) -> Result<Option<WorldState>, SnapshotError> {
        self.push_decoded(index, total, data)
    }

    /// Добавить часть и разобрать собранные байты как `T` (мир, рельеф).
    pub fn push_decoded<T: serde::de::DeserializeOwned>(
        &mut self,
        index: u16,
        total: u16,
        data: Vec<u8>,
    ) -> Result<Option<T>, SnapshotError> {
        match self.total {
            None => {
                self.total = Some(total);
                self.parts = vec![None; usize::from(total)];
            }
            Some(t) if t != total => {
                return Err(SnapshotError::Inconsistent {
                    expected: t,
                    got: total,
                });
            }
            Some(_) => {}
        }
        let slot = self
            .parts
            .get_mut(usize::from(index))
            .ok_or(SnapshotError::Conflicting(index))?;
        match slot {
            Some(existing) if *existing != data => return Err(SnapshotError::Conflicting(index)),
            Some(_) => {}
            None => *slot = Some(data),
        }
        if self.parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let bytes: Vec<u8> = self.parts.drain(..).flatten().flatten().collect();
        self.total = None;
        postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|e| SnapshotError::Decode(e.to_string()))
    }

    pub fn reset(&mut self) {
        self.total = None;
        self.parts.clear();
    }
}

#[cfg(test)]
mod tests {
    use astrocore_world::{FloorDef, FloorId, World, WorldCommand, WorldConfig};

    use super::*;

    fn big_state() -> WorldState {
        let mut w = World::new("big", WorldConfig::default()).unwrap();
        for f in 0..2000 {
            w.apply(WorldCommand::DefineFloor(FloorDef {
                id: FloorId(f),
                name: format!("a-fairly-long-floor-name-number-{f}"),
                base_y: f as f32,
                height: 1.0,
            }))
            .unwrap();
        }
        w.to_state()
    }

    fn parts(state: &WorldState) -> Vec<(u16, u16, Vec<u8>)> {
        split(state)
            .unwrap()
            .into_iter()
            .map(|m| match m {
                ServerMessage::SnapshotPart { index, total, data } => (index, total, data),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn large_state_splits_and_reassembles_in_any_order_with_duplicates() {
        let state = big_state();
        let mut p = parts(&state);
        assert!(
            p.len() > 1,
            "состояние не помещается в один кусок: {}",
            p.len()
        );
        assert!(
            p.iter()
                .all(|(_, t, d)| *t as usize == p.len() && d.len() <= MAX_SNAPSHOT_PART_BYTES)
        );
        p.reverse();
        let mut r = Reassembler::new();
        let mut out = None;
        for (i, t, d) in p.iter().cloned().chain(std::iter::once(p[0].clone())) {
            if out.is_none() {
                out = r.push(i, t, d).unwrap();
            }
        }
        assert_eq!(out.unwrap(), state);
    }

    #[test]
    fn incomplete_and_inconsistent_streams_are_detected() {
        let p = parts(&big_state());
        let mut r = Reassembler::new();
        assert_eq!(r.push(p[0].0, p[0].1, p[0].2.clone()).unwrap(), None);
        assert_eq!(
            r.push(0, p[0].1 + 1, vec![]),
            Err(SnapshotError::Inconsistent {
                expected: p[0].1,
                got: p[0].1 + 1
            })
        );
        assert_eq!(
            r.push(0, p[0].1, vec![1, 2, 3]),
            Err(SnapshotError::Conflicting(0))
        );
        assert_eq!(
            r.push(p[0].1, p[0].1, vec![]),
            Err(SnapshotError::Conflicting(p[0].1)),
            "индекс за пределами"
        );
    }

    #[test]
    fn corrupted_payload_is_a_decode_error() {
        let mut r = Reassembler::new();
        assert!(matches!(
            r.push(0, 1, vec![0xFF; 64]),
            Err(SnapshotError::Decode(_))
        ));
        // После ошибки сборщик пригоден для нового снимка.
        let state = big_state();
        let p = parts(&state);
        let mut r = Reassembler::new();
        let mut out = None;
        for (i, t, d) in p {
            out = r.push(i, t, d).unwrap();
        }
        assert_eq!(out.unwrap(), state);
    }
}
