use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use astrocore_core::PrototypeId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Лимит размера одного ассета при хешировании, байты.
pub const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// SHA-256 содержимого, 32 байта.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Хеш последовательности хешей (для хеша зависимостей). Порядок важен.
    pub fn of_hashes<'a>(hashes: impl IntoIterator<Item = &'a ContentHash>) -> Self {
        let mut h = Sha256::new();
        for x in hashes {
            h.update(x.0);
        }
        Self(h.finalize().into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", &self.to_hex()[..12])
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Image,
    Model,
    SpriteSet,
    Prototype,
    Audio,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetEntry {
    pub id: PrototypeId,
    pub kind: AssetKind,
    /// Путь относительно корня ассетов, всегда с `/`.
    pub source: String,
    pub hash: ContentHash,
    pub license: Option<String>,
    pub attribution: Option<String>,
    /// Цепочка преобразований («atlas-pack», «srgb→linear» …).
    pub transforms: Vec<String>,
}

impl AssetEntry {
    pub fn has_license(&self) -> bool {
        self.license
            .as_deref()
            .is_some_and(|l| !l.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    #[error("небезопасный путь ассета {0:?}")]
    UnsafePath(String),
    #[error("ассет {0} уже есть в манифесте")]
    Duplicate(PrototypeId),
    #[error("ассет {0} не найден")]
    Missing(PrototypeId),
    #[error("хеш ассета {id} не совпадает: в манифесте {expected}, фактический {actual}")]
    HashMismatch {
        id: PrototypeId,
        expected: ContentHash,
        actual: ContentHash,
    },
    #[error("ассет {id} превышает лимит ({size} байт)")]
    TooLarge { id: PrototypeId, size: u64 },
    #[error("ошибка чтения {path:?}: {reason}")]
    Io { path: String, reason: String },
    #[error("манифест: {0}")]
    Manifest(String),
    #[error("экспорт заблокирован: ассеты без лицензии: {0:?}")]
    UnlicensedExport(Vec<PrototypeId>),
}

/// Проверить относительный путь: без `..`, без абсолютных путей и корней, без обратных слэшей.
pub fn check_relative_path(path: &str) -> Result<(), AssetError> {
    let bad = || AssetError::UnsafePath(path.to_string());
    if path.is_empty()
        || path.contains(['\\', '\0', ':'])
        || path.starts_with('/')
        || path.len() > 512
    {
        return Err(bad());
    }
    // Сегменты проверяются явно: `Path::components` молча схлопывает `a/./b`.
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(bad());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetManifest {
    entries: BTreeMap<PrototypeId, AssetEntry>,
}

#[derive(Serialize, Deserialize)]
struct ManifestFile {
    schema_version: u32,
    assets: Vec<AssetEntry>,
}

impl AssetManifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: AssetEntry) -> Result<(), AssetError> {
        check_relative_path(&entry.source)?;
        if self.entries.contains_key(&entry.id) {
            return Err(AssetError::Duplicate(entry.id));
        }
        self.entries.insert(entry.id.clone(), entry);
        Ok(())
    }

    pub fn get(&self, id: &PrototypeId) -> Option<&AssetEntry> {
        self.entries.get(id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &AssetEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Прочитать файл ассета и добавить запись с вычисленным хешем.
    pub fn add_file(
        &mut self,
        root: &Path,
        id: PrototypeId,
        kind: AssetKind,
        source: &str,
        license: Option<String>,
        attribution: Option<String>,
    ) -> Result<(), AssetError> {
        let bytes = read_limited(root, source, &id)?;
        self.insert(AssetEntry {
            id,
            kind,
            source: source.to_string(),
            hash: ContentHash::of(&bytes),
            license,
            attribution,
            transforms: Vec::new(),
        })
    }

    /// Проверить, что файлы на диске соответствуют манифесту.
    pub fn verify_files(&self, root: &Path) -> Result<(), AssetError> {
        for e in self.entries.values() {
            let bytes = read_limited(root, &e.source, &e.id)?;
            let actual = ContentHash::of(&bytes);
            if actual != e.hash {
                return Err(AssetError::HashMismatch {
                    id: e.id.clone(),
                    expected: e.hash,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Хеш всего манифеста (идентификаторы + хеши в порядке идентификаторов) —
    /// «хеш зависимостей» карты.
    pub fn dependency_hash(&self) -> ContentHash {
        let mut h = Sha256::new();
        for e in self.entries.values() {
            h.update(e.id.as_str().as_bytes());
            h.update([0]);
            h.update(e.hash.0);
        }
        ContentHash(h.finalize().into())
    }

    /// Ассеты без лицензии.
    pub fn unlicensed(&self) -> Vec<PrototypeId> {
        self.entries
            .values()
            .filter(|e| !e.has_license())
            .map(|e| e.id.clone())
            .collect()
    }

    /// Политика экспорта: публичный пакет с неизвестной лицензией запрещён.
    pub fn check_public_export(&self) -> Result<(), AssetError> {
        let bad = self.unlicensed();
        if bad.is_empty() {
            Ok(())
        } else {
            Err(AssetError::UnlicensedExport(bad))
        }
    }

    pub fn to_json(&self) -> String {
        let file = ManifestFile {
            schema_version: MANIFEST_SCHEMA_VERSION,
            assets: self.entries.values().cloned().collect(),
        };
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Self, AssetError> {
        let file: ManifestFile =
            serde_json::from_str(text).map_err(|e| AssetError::Manifest(e.to_string()))?;
        if file.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(AssetError::Manifest(format!(
                "версия схемы {} не поддерживается",
                file.schema_version
            )));
        }
        let mut m = Self::new();
        for e in file.assets {
            m.insert(e)?;
        }
        Ok(m)
    }
}

fn read_limited(root: &Path, source: &str, id: &PrototypeId) -> Result<Vec<u8>, AssetError> {
    check_relative_path(source)?;
    let path: PathBuf = root.join(source);
    let io = |e: std::io::Error| AssetError::Io {
        path: source.to_string(),
        reason: e.to_string(),
    };
    let meta = std::fs::metadata(&path).map_err(io)?;
    if meta.len() > MAX_ASSET_BYTES {
        return Err(AssetError::TooLarge {
            id: id.clone(),
            size: meta.len(),
        });
    }
    std::fs::read(&path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(s: &str) -> PrototypeId {
        PrototypeId::new(s).unwrap()
    }

    #[test]
    fn hashes_are_stable_and_content_sensitive() {
        assert_eq!(ContentHash::of(b"abc"), ContentHash::of(b"abc"));
        assert_ne!(ContentHash::of(b"abc"), ContentHash::of(b"abd"));
        // Известный вектор SHA-256("abc").
        assert_eq!(
            ContentHash::of(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let (a, b) = (ContentHash::of(b"1"), ContentHash::of(b"2"));
        assert_ne!(
            ContentHash::of_hashes([&a, &b]),
            ContentHash::of_hashes([&b, &a]),
            "порядок важен"
        );
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a\\b",
            "a/./b",
            "C:/x",
            "a\0b",
        ] {
            assert!(check_relative_path(bad).is_err(), "{bad:?}");
        }
        for ok in ["a.png", "sprites/mob/idle.png", "x/y/z.glb"] {
            assert!(check_relative_path(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn manifest_hashes_files_and_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"png-bytes").unwrap();
        let mut m = AssetManifest::new();
        m.add_file(
            dir.path(),
            pid("t:a"),
            AssetKind::Image,
            "a.png",
            Some("CC0".into()),
            None,
        )
        .unwrap();
        m.verify_files(dir.path()).unwrap();
        let before = m.dependency_hash();

        std::fs::write(dir.path().join("a.png"), b"changed").unwrap();
        assert!(matches!(
            m.verify_files(dir.path()),
            Err(AssetError::HashMismatch { .. })
        ));

        let mut m2 = AssetManifest::new();
        m2.add_file(
            dir.path(),
            pid("t:a"),
            AssetKind::Image,
            "a.png",
            Some("CC0".into()),
            None,
        )
        .unwrap();
        assert_ne!(
            m2.dependency_hash(),
            before,
            "хеш зависимостей отражает содержимое"
        );
    }

    #[test]
    fn duplicates_missing_files_and_traversal_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"x").unwrap();
        let mut m = AssetManifest::new();
        m.add_file(
            dir.path(),
            pid("t:a"),
            AssetKind::Image,
            "a.png",
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            m.add_file(
                dir.path(),
                pid("t:a"),
                AssetKind::Image,
                "a.png",
                None,
                None
            ),
            Err(AssetError::Duplicate(_))
        ));
        assert!(matches!(
            m.add_file(
                dir.path(),
                pid("t:b"),
                AssetKind::Image,
                "missing.png",
                None,
                None
            ),
            Err(AssetError::Io { .. })
        ));
        assert!(matches!(
            m.add_file(
                dir.path(),
                pid("t:c"),
                AssetKind::Image,
                "../outside.png",
                None,
                None
            ),
            Err(AssetError::UnsafePath(_))
        ));
    }

    #[test]
    fn unlicensed_assets_block_public_export() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"1").unwrap();
        std::fs::write(dir.path().join("b.png"), b"2").unwrap();
        let mut m = AssetManifest::new();
        m.add_file(
            dir.path(),
            pid("t:a"),
            AssetKind::Image,
            "a.png",
            Some("CC-BY-SA-3.0".into()),
            Some("Автор".into()),
        )
        .unwrap();
        assert!(m.check_public_export().is_ok());
        m.add_file(
            dir.path(),
            pid("t:b"),
            AssetKind::Image,
            "b.png",
            Some("  ".into()),
            None,
        )
        .unwrap();
        assert_eq!(
            m.check_public_export(),
            Err(AssetError::UnlicensedExport(vec![pid("t:b")]))
        );
    }

    #[test]
    fn manifest_json_roundtrip_and_hostile_input() {
        let mut m = AssetManifest::new();
        m.insert(AssetEntry {
            id: pid("t:a"),
            kind: AssetKind::Model,
            source: "m/a.glb".into(),
            hash: ContentHash::of(b"x"),
            license: Some("MIT".into()),
            attribution: None,
            transforms: vec!["optimize".into()],
        })
        .unwrap();
        assert_eq!(AssetManifest::from_json(&m.to_json()).unwrap(), m);
        assert!(AssetManifest::from_json("{").is_err());
        let evil = m.to_json().replace("m/a.glb", "../../etc/passwd");
        assert!(matches!(
            AssetManifest::from_json(&evil),
            Err(AssetError::UnsafePath(_))
        ));
        let wrong = m
            .to_json()
            .replace("\"schema_version\": 1", "\"schema_version\": 4");
        assert!(AssetManifest::from_json(&wrong).is_err());
    }
}
