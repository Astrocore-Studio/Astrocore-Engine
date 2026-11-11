mod migrate;

pub use migrate::{Migrator, SNAPSHOT_FORMAT_VERSION};

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use astrocore_world::{ReplicaOutcome, World, WorldError, WorldOp, WorldState};
use serde::{Deserialize, Serialize};

const SNAPSHOT_FILE: &str = "snapshot.json";
const SNAPSHOT_TMP: &str = "snapshot.json.tmp";
const JOURNAL_FILE: &str = "journal.jsonl";

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] io::Error),
    #[error("некорректный JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("мир отклонил данные: {0}")]
    World(#[from] WorldError),
    #[error("миграция: {0}")]
    Migration(String),
    #[error("хеш содержимого снимка не совпадает (ожидался {expected:#x}, получен {actual:#x})")]
    HashMismatch { expected: u64, actual: u64 },
    #[error("ревизия в снимке ({snapshot}) не совпадает с ревизией мира ({world})")]
    RevisionMismatch { snapshot: u64, world: u64 },
    #[error("журнал повреждён в строке {line}: {reason}")]
    CorruptJournal { line: usize, reason: String },
    #[error("журнал без снимка: восстановление невозможно")]
    JournalWithoutSnapshot,
    #[error("пропуск в журнале: ожидалась операция {expected}, найдена {got}")]
    JournalGap { expected: u64, got: u64 },
    #[error("операция журнала {seq} не применима к снимку: {source}")]
    JournalRejected { seq: u64, source: WorldError },
}

#[derive(Serialize, Deserialize)]
struct SnapshotFile {
    format_version: u32,
    world_revision: u64,
    content_hash: u64,
    world: WorldState,
}

/// Результат восстановления.
#[derive(Debug)]
pub struct Recovered {
    pub world: World,
    /// Сколько операций журнала было реально применено.
    pub replayed: usize,
    /// Был ли отброшен оборванный последний фрагмент журнала (сбой во время записи).
    pub discarded_torn_tail: bool,
}

/// Хранилище мира в каталоге.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Store, PersistError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn has_snapshot(&self) -> bool {
        self.dir.join(SNAPSHOT_FILE).is_file()
    }

    /// Записать контрольную точку и очистить журнал (операции уже учтены в снимке).
    ///
    /// Порядок обеспечивает безопасность при сбое в любой момент: пока `rename` не
    /// выполнен, действует старый снимок + журнал; после — новый снимок, а оставшийся
    /// журнал безвреден, потому что повтор пропускает уже учтённые операции.
    pub fn save_snapshot(&self, world: &World) -> Result<(), PersistError> {
        let _span = tracing::info_span!("save_snapshot").entered();
        let file = SnapshotFile {
            format_version: SNAPSHOT_FORMAT_VERSION,
            world_revision: world.revision().0,
            content_hash: world.content_hash(),
            world: world.to_state(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        let tmp = self.dir.join(SNAPSHOT_TMP);
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.dir.join(SNAPSHOT_FILE))?;
        sync_dir(&self.dir);
        let journal = File::create(self.dir.join(JOURNAL_FILE))?;
        journal.sync_all()?;
        Ok(())
    }

    /// Дописать подтверждённую операцию в журнал.
    pub fn append(&self, op: &WorldOp) -> Result<(), PersistError> {
        let json = serde_json::to_string(op)?;
        let crc = crc32fast::hash(json.as_bytes());
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(JOURNAL_FILE))?;
        writeln!(f, "{crc:08x} {json}")?;
        f.sync_data()?;
        Ok(())
    }

    /// Восстановить мир. `Ok(None)` — сохранений нет (первый запуск).
    pub fn recover(&self) -> Result<Option<Recovered>, PersistError> {
        let _span = tracing::info_span!("recover").entered();
        let snapshot_path = self.dir.join(SNAPSHOT_FILE);
        if !snapshot_path.is_file() {
            return if self.journal_has_entries()? {
                Err(PersistError::JournalWithoutSnapshot)
            } else {
                Ok(None)
            };
        }
        let mut world = self.load_snapshot(&snapshot_path)?;
        let (ops, torn) = self.read_journal()?;
        let mut replayed = 0;
        for op in ops {
            match world.apply_replicated(&op) {
                ReplicaOutcome::Applied => replayed += 1,
                ReplicaOutcome::Duplicate => {}
                ReplicaOutcome::Gap { expected, got } => {
                    return Err(PersistError::JournalGap { expected, got });
                }
                ReplicaOutcome::Rejected(source) => {
                    return Err(PersistError::JournalRejected {
                        seq: op.seq,
                        source,
                    });
                }
            }
        }
        Ok(Some(Recovered {
            world,
            replayed,
            discarded_torn_tail: torn,
        }))
    }

    fn load_snapshot(&self, path: &Path) -> Result<World, PersistError> {
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        let value = Migrator::current().migrate(value)?;
        let file: SnapshotFile = serde_json::from_value(value)?;
        let world = World::from_state(file.world)?;
        if world.revision().0 != file.world_revision {
            return Err(PersistError::RevisionMismatch {
                snapshot: file.world_revision,
                world: world.revision().0,
            });
        }
        let actual = world.content_hash();
        if actual != file.content_hash {
            return Err(PersistError::HashMismatch {
                expected: file.content_hash,
                actual,
            });
        }
        Ok(world)
    }

    fn journal_has_entries(&self) -> Result<bool, PersistError> {
        match fs::metadata(self.dir.join(JOURNAL_FILE)) {
            Ok(m) => Ok(m.len() > 0),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Прочитать журнал. Оборванная последняя строка (нет `\n` или не сходится CRC)
    /// — следствие сбоя при записи и тихо отбрасывается; повреждение в середине — ошибка.
    fn read_journal(&self) -> Result<(Vec<WorldOp>, bool), PersistError> {
        let f = match File::open(self.dir.join(JOURNAL_FILE)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
            Err(e) => return Err(e.into()),
        };
        let lines: Vec<String> = BufReader::new(f).lines().collect::<Result<_, _>>()?;
        let mut ops = Vec::with_capacity(lines.len());
        let mut torn = false;
        for (i, line) in lines.iter().enumerate() {
            match parse_journal_line(line) {
                Ok(op) => ops.push(op),
                Err(reason) if i + 1 == lines.len() => {
                    let _ = reason;
                    torn = true;
                }
                Err(reason) => {
                    return Err(PersistError::CorruptJournal {
                        line: i + 1,
                        reason,
                    });
                }
            }
        }
        Ok((ops, torn))
    }
}

fn parse_journal_line(line: &str) -> Result<WorldOp, String> {
    let (crc_hex, json) = line.split_once(' ').ok_or("нет разделителя CRC")?;
    let crc = u32::from_str_radix(crc_hex, 16).map_err(|e| format!("CRC: {e}"))?;
    if crc32fast::hash(json.as_bytes()) != crc {
        return Err("контрольная сумма не совпала".into());
    }
    serde_json::from_str(json).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests;
