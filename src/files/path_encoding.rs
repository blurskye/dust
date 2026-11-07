//! Lossless path encoding for saved records.

// Keep existing UTF-8 paths readable; Unix paths with other bytes use byte arrays.

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Encoded {
    Text(String),
    Bytes(Vec<u8>),
    Wide { wide: Vec<u16> },
}
impl Encoded {
    fn from_path(path: &Path) -> Self {
        match path.to_str() {
            Some(text) => Self::Text(text.to_owned()),
            #[cfg(unix)]
            None => Self::Bytes(path.as_os_str().as_bytes().to_vec()),
            #[cfg(windows)]
            None => {
                use std::os::windows::ffi::OsStrExt;
                Self::Wide {
                    wide: path.as_os_str().encode_wide().collect(),
                }
            }
        }
    }
    fn into_path(self) -> Result<PathBuf, String> {
        match self {
            Self::Text(text) => Ok(PathBuf::from(text)),
            #[cfg(unix)]
            Self::Bytes(bytes) => Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes))),
            #[cfg(windows)]
            Self::Bytes(bytes) => String::from_utf8(bytes)
                .map(PathBuf::from)
                .map_err(|e| e.to_string()),
            #[cfg(windows)]
            Self::Wide { wide } => {
                use std::os::windows::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
            }
            #[cfg(unix)]
            Self::Wide { wide } => String::from_utf16(&wide)
                .map(PathBuf::from)
                .map_err(|e| e.to_string()),
        }
    }
}
pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    Encoded::from_path(path).serialize(serializer)
}
pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    Encoded::deserialize(deserializer)?
        .into_path()
        .map_err(serde::de::Error::custom)
}
pub(crate) fn encode(path: &Path) -> Result<String, serde_json::Error> {
    #[cfg(unix)]
    let encoded = Encoded::Bytes(path.as_os_str().as_bytes().to_vec());
    #[cfg(windows)]
    let encoded = Encoded::from_path(path);
    serde_json::to_string(&encoded)
}

pub(crate) fn decode(text: &str) -> Result<PathBuf, String> {
    serde_json::from_str::<Encoded>(text)
        .map_err(|e| e.to_string())?
        .into_path()
}

pub mod optional {
    use crate::files::path_encoding::*;
    pub fn serialize<S: serde::Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref()
            .map(Encoded::from_path)
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Option::<Encoded>::deserialize(deserializer)?
            .map(Encoded::into_path)
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}
pub mod list {
    use crate::files::path_encoding::*;
    pub fn serialize<S: serde::Serializer>(
        paths: &[PathBuf],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        paths
            .iter()
            .map(|path| Encoded::from_path(path))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PathBuf>, D::Error> {
        Vec::<Encoded>::deserialize(deserializer)?
            .into_iter()
            .map(Encoded::into_path)
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::de::Error::custom)
    }
}
