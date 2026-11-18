//! Where Dusty keeps its configuration and state, honouring `DUSTY_CONFIG_HOME`,
//! `DUSTY_STATE_HOME` and each platform's usual folders.

use std::env;
use std::path::PathBuf;

pub fn user_state_directory() -> PathBuf {
    if let Some(root) = env::var_os("DUSTY_STATE_HOME") {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dusty")
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("XDG_STATE_HOME")
            .map(|root| PathBuf::from(root).join("dusty"))
            .or_else(|| {
                env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support/Dusty"))
            })
            .unwrap_or_else(|| PathBuf::from("dusty"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dusty")
    }
}

#[must_use]
pub fn user_config_path() -> PathBuf {
    if let Some(root) = env::var_os("DUSTY_CONFIG_HOME") {
        return PathBuf::from(root).join("config.toml");
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dusty")
            .join("config.toml")
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(|root| PathBuf::from(root).join("dusty"))
            .or_else(|| {
                env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support/Dusty"))
            })
            .unwrap_or_else(|| PathBuf::from("dusty"))
            .join("config.toml")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dusty/config.toml")
    }
}
