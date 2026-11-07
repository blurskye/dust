//! Lossless path encoding for saved records.

// Keep existing UTF-8 paths readable; Unix paths with other bytes use byte arrays.

use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Encoded {
    Text(String),
    Bytes(Vec<u8>),
}
impl Encoded {
    fn from_path(path: &Path) -> Self {
        match path.to_str() {
            Some(text) => Self::Text(text.to_owned()),
            None => Self::Bytes(path.as_os_str().as_bytes().to_vec()),
        }
    }
    fn into_path(self) -> PathBuf {
        match self {
            Self::Text(text) => PathBuf::from(text),
            Self::Bytes(bytes) => PathBuf::from(std::ffi::OsString::from_vec(bytes)),
        }
    }
}
pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    Encoded::from_path(path).serialize(serializer)
}
pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    Encoded::deserialize(deserializer).map(Encoded::into_path)
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
        Option::<Encoded>::deserialize(deserializer).map(|path| path.map(Encoded::into_path))
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
        Vec::<Encoded>::deserialize(deserializer)
            .map(|paths| paths.into_iter().map(Encoded::into_path).collect())
    }
}
