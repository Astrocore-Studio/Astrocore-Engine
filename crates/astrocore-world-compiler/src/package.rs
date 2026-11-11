//! Контейнер пакета: `MAGIC | вид | версия схемы | длина | полезная нагрузка | SHA-256`.

use astrocore_assets::ContentHash;
use serde::{Deserialize, Serialize};

pub const MAGIC: [u8; 4] = *b"ACPK";
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;
/// Верхняя граница размера полезной нагрузки при чтении.
pub const MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const HEADER_LEN: usize = 4 + 1 + 4 + 8;
const HASH_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Client = 1,
    Server = 2,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("не пакет Astrocore (неверная сигнатура)")]
    BadMagic,
    #[error("пакет слишком короткий")]
    Truncated,
    #[error("ожидался другой вид пакета")]
    WrongKind,
    #[error("версия схемы пакета {0} не поддерживается")]
    UnsupportedVersion(u32),
    #[error("заявленный размер {0} превышает лимит")]
    TooLarge(u64),
    #[error("контрольная сумма пакета не совпала")]
    HashMismatch,
    #[error("ошибка разбора: {0}")]
    Decode(String),
    #[error("ошибка кодирования: {0}")]
    Encode(String),
}

pub fn encode_package<T: Serialize>(
    kind: PackageKind,
    payload: &T,
) -> Result<Vec<u8>, PackageError> {
    let body = postcard::to_stdvec(payload).map_err(|e| PackageError::Encode(e.to_string()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len() + HASH_LEN);
    out.extend_from_slice(&MAGIC);
    out.push(kind as u8);
    out.extend_from_slice(&PACKAGE_SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(&body);
    let hash = ContentHash::of(&out);
    out.extend_from_slice(hash.as_bytes());
    Ok(out)
}

/// Разобрать недоверенный пакет: сигнатура, вид, версия, лимит, хеш — до десериализации.
/// Возвращает нагрузку и хеш пакета.
pub fn decode_package<T: for<'de> Deserialize<'de>>(
    kind: PackageKind,
    bytes: &[u8],
) -> Result<(T, ContentHash), PackageError> {
    if bytes.len() < HEADER_LEN + HASH_LEN {
        return Err(PackageError::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(PackageError::BadMagic);
    }
    if bytes[4] != kind as u8 {
        return Err(PackageError::WrongKind);
    }
    let version = u32::from_le_bytes(bytes[5..9].try_into().expect("длина проверена"));
    if version != PACKAGE_SCHEMA_VERSION {
        return Err(PackageError::UnsupportedVersion(version));
    }
    let len = u64::from_le_bytes(bytes[9..17].try_into().expect("длина проверена"));
    if len > MAX_PAYLOAD_BYTES {
        return Err(PackageError::TooLarge(len));
    }
    let total = (HEADER_LEN as u64)
        .checked_add(len)
        .and_then(|v| v.checked_add(HASH_LEN as u64));
    if total != Some(bytes.len() as u64) {
        return Err(PackageError::Truncated);
    }
    let (content, tail) = bytes.split_at(bytes.len() - HASH_LEN);
    let actual = ContentHash::of(content);
    if actual.as_bytes() != tail {
        return Err(PackageError::HashMismatch);
    }
    let payload = postcard::from_bytes(&content[HEADER_LEN..])
        .map_err(|e| PackageError::Decode(e.to_string()))?;
    Ok((payload, actual))
}

/// Хеш пакета — SHA-256 всего, кроме завершающей контрольной суммы (то же значение,
/// что хранится в хвосте). `None`, если пакет короче контрольной суммы.
pub fn package_hash(bytes: &[u8]) -> Option<ContentHash> {
    bytes
        .len()
        .checked_sub(HASH_LEN)
        .map(|n| ContentHash::of(&bytes[..n]))
}
