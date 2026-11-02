//! Bounded, data-only theme discovery on the existing configuration worker.
use super::ThemeConfig;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::SyncSender,
};

pub(super) const BUNDLED: &[(&str, &str)] = &[
    ("dust", super::load::BUNDLED_THEME),
    (
        "catppuccin-latte",
        include_str!("../../assets/themes/catppuccin-latte.toml"),
    ),
    (
        "catppuccin-frappe",
        include_str!("../../assets/themes/catppuccin-frappe.toml"),
    ),
    (
        "catppuccin-macchiato",
        include_str!("../../assets/themes/catppuccin-macchiato.toml"),
    ),
    (
        "catppuccin-mocha",
        include_str!("../../assets/themes/catppuccin-mocha.toml"),
    ),
    (
        "gruvbox-dark",
        include_str!("../../assets/themes/gruvbox-dark.toml"),
    ),
    ("nord", include_str!("../../assets/themes/nord.toml")),
    (
        "tokyo-night",
        include_str!("../../assets/themes/tokyo-night.toml"),
    ),
    (
        "tokyo-night-storm",
        include_str!("../../assets/themes/tokyo-night-storm.toml"),
    ),
    (
        "tokyo-night-moon",
        include_str!("../../assets/themes/tokyo-night-moon.toml"),
    ),
    (
        "tokyo-night-day",
        include_str!("../../assets/themes/tokyo-night-day.toml"),
    ),
    (
        "rose-pine",
        include_str!("../../assets/themes/rose-pine.toml"),
    ),
    (
        "rose-pine-moon",
        include_str!("../../assets/themes/rose-pine-moon.toml"),
    ),
    (
        "rose-pine-dawn",
        include_str!("../../assets/themes/rose-pine-dawn.toml"),
    ),
];
pub(super) fn bundled_named(name: &str) -> Option<ThemeChoice> {
    let (_, source) = BUNDLED.iter().find(|(id, _)| *id == name)?;
    Some(ThemeChoice {
        theme: toml::from_str(source).expect("bundled themes are validated by tests"),
        path: format!("<bundled:theme:{name}>").into(),
    })
}
pub(crate) fn theme_display_name(name: &str) -> String {
    match name {
        "dust" => "Dust",
        "catppuccin-latte" => "Catppuccin Latte",
        "catppuccin-frappe" => "Catppuccin Frappé",
        "catppuccin-macchiato" => "Catppuccin Macchiato",
        "catppuccin-mocha" => "Catppuccin Mocha",
        "gruvbox-dark" => "Gruvbox Dark",
        "nord" => "Nord",
        "tokyo-night" => "Tokyo Night",
        "tokyo-night-storm" => "Tokyo Night Storm",
        "tokyo-night-moon" => "Tokyo Night Moon",
        "tokyo-night-day" => "Tokyo Night Day",
        "rose-pine" => "Rosé Pine",
        "rose-pine-moon" => "Rosé Pine Moon",
        "rose-pine-dawn" => "Rosé Pine Dawn",
        other => other,
    }
    .into()
}

pub(crate) struct ThemeChoice {
    pub(crate) theme: ThemeConfig,
    pub(crate) path: PathBuf,
}
pub(crate) struct ThemeCatalog {
    pub(crate) choices: Vec<ThemeChoice>,
    pub(crate) diagnostics: Vec<String>,
}
pub(crate) enum ThemeReadSource {
    Directory(Option<PathBuf>),
    Saved(String),
}
pub(crate) struct ThemeRead {
    pub(crate) source: ThemeReadSource,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) reply: SyncSender<ThemeCatalog>,
}
pub(super) fn discover(request: ThemeRead) {
    if request.cancelled.load(Ordering::Acquire) {
        return;
    }
    if let ThemeReadSource::Saved(source) = &request.source {
        let result = (|| {
            if source.len() > 128 * 1024 {
                return Err("saved theme exceeds 128 KiB".to_string());
            }
            // A bundled theme comes from this build, so a copy saved by an older one, with
            // fields since removed, still restores and picks up the new colours.
            #[derive(serde::Deserialize)]
            struct Named {
                name: String,
            }
            let bundled = serde_json::from_str::<Named>(source)
                .ok()
                .and_then(|saved| bundled_named(&saved.name));
            let theme: ThemeConfig = match bundled {
                Some(choice) => choice.theme,
                None => serde_json::from_str(source)
                    .map_err(|e| format!("invalid saved theme: {e}"))?,
            };
            if theme.name.is_empty()
                || theme.name.len() > 96
                || theme.name.chars().any(|c| c.is_control())
            {
                return Err("invalid saved theme name".into());
            }
            theme.validate()?;
            Ok(ThemeChoice {
                path: format!("<editor-state:theme:{}>", theme.name).into(),
                theme,
            })
        })();
        let catalog = match result {
            Ok(choice) => ThemeCatalog {
                choices: vec![choice],
                diagnostics: vec![],
            },
            Err(error) => ThemeCatalog {
                choices: vec![],
                diagnostics: vec![
                    error
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(512)
                        .collect(),
                ],
            },
        };
        if !request.cancelled.load(Ordering::Acquire) {
            let _ = request.reply.try_send(catalog);
        }
        return;
    }
    let mut catalog = ThemeCatalog {
        choices: BUNDLED
            .iter()
            .filter_map(|(name, _)| bundled_named(name))
            .collect(),
        diagnostics: Vec::new(),
    };
    if let ThemeReadSource::Directory(Some(directory)) = &request.source {
        match fs::read_dir(directory) {
            Ok(entries) => {
                let mut paths = Vec::new();
                for (index, entry) in entries.enumerate() {
                    if request.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    if index == 256 {
                        catalog.diagnostics.push("Theme directory exceeds 256 entries; remaining files were not inspected".into());
                        break;
                    }
                    match entry {
                        Ok(entry) if entry.path().extension().is_some_and(|ext| ext == "toml") => {
                            paths.push(entry.path())
                        }
                        Ok(_) => {}
                        Err(error) => catalog.diagnostics.push(error.to_string()),
                    }
                }
                paths.sort();
                for (index, path) in paths.into_iter().enumerate() {
                    if request.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    if index == 31 {
                        catalog.diagnostics.push(
                            "Theme discovery limited to 31 installed files plus bundled themes"
                                .into(),
                        );
                        break;
                    }
                    match read_theme(&path) {
                        Ok(theme) => catalog.choices.push(ThemeChoice { theme, path }),
                        Err(error) => catalog
                            .diagnostics
                            .push(format!("{}: {error}", path.display())),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => catalog
                .diagnostics
                .push(format!("{}: {error}", directory.display())),
        }
    }
    for diagnostic in &mut catalog.diagnostics {
        *diagnostic = diagnostic
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
    }
    if !request.cancelled.load(Ordering::Acquire) {
        let _ = request.reply.try_send(catalog);
    }
}
fn read_theme(path: &Path) -> Result<ThemeConfig, String> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("theme filename is not UTF-8")?;
    if name.is_empty()
        || name.len() > 96
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err("theme filename must be a bounded name using letters, digits, - or _".into());
    }
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("theme must be a regular file, not a symlink or device".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    const MAXIMUM: u64 = 128 * 1024;
    if !metadata.is_file() || metadata.len() > MAXIMUM {
        return Err("theme is not a regular file within 128 KiB".into());
    }
    let mut source = String::new();
    file.take(MAXIMUM + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() as u64 > MAXIMUM {
        return Err("theme exceeds 128 KiB".into());
    }
    let theme: ThemeConfig = toml::from_str(&source).map_err(|e| e.to_string())?;
    if theme.name != name {
        return Err("declared theme name differs from its filename".into());
    }
    theme.validate()?;
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn discovery_validates_bounds_names_styles_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let valid =
            super::super::load::BUNDLED_THEME.replace("name = \"dust\"", "name = \"valid\"");
        fs::write(root.path().join("valid.toml"), &valid).unwrap();
        fs::write(root.path().join("mismatch.toml"), &valid).unwrap();
        fs::write(root.path().join("broken.toml"), "not toml").unwrap();
        fs::write(root.path().join("large.toml"), "x".repeat(128 * 1024 + 1)).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.path().join("valid.toml"),
                root.path().join("link.toml"),
            )
            .unwrap();
            nix::unistd::mkfifo(
                &root.path().join("pipe.toml"),
                nix::sys::stat::Mode::S_IRUSR,
            )
            .unwrap();
        }
        let mut broken_style = super::super::load::bundled_theme();
        broken_style.name = "style".into();
        broken_style.styles.accent.foreground_16 = None;
        fs::write(
            root.path().join("style.toml"),
            toml::to_string(&broken_style).unwrap(),
        )
        .unwrap();
        let (reply, receiver) = mpsc::sync_channel(1);
        discover(ThemeRead {
            source: ThemeReadSource::Directory(Some(root.path().into())),
            cancelled: Arc::new(AtomicBool::new(false)),
            reply,
        });
        let catalog = receiver.try_recv().unwrap();
        assert_eq!(
            catalog
                .choices
                .iter()
                .map(|c| c.theme.name.as_str())
                .collect::<Vec<_>>(),
            BUNDLED
                .iter()
                .map(|(name, _)| *name)
                .chain(["valid"])
                .collect::<Vec<_>>()
        );
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|error| error.contains("16-color fallback"))
        );
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|error| error.contains("filename"))
        );
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|error| error.contains("128 KiB"))
        );
        assert!(
            catalog
                .diagnostics
                .iter()
                .all(|error| error.chars().count() <= 512)
        );
        let (reply, receiver) = mpsc::sync_channel(1);
        discover(ThemeRead {
            source: ThemeReadSource::Directory(Some(root.path().into())),
            cancelled: Arc::new(AtomicBool::new(true)),
            reply,
        });
        assert!(receiver.try_recv().is_err());
        for index in 0..40 {
            let name = format!("a{index:02}");
            fs::write(
                root.path().join(format!("{name}.toml")),
                valid.replace("\"valid\"", &format!("\"{name}\"")),
            )
            .unwrap();
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        discover(ThemeRead {
            source: ThemeReadSource::Directory(Some(root.path().into())),
            cancelled: Arc::new(AtomicBool::new(false)),
            reply,
        });
        let catalog = receiver.try_recv().unwrap();
        assert_eq!(catalog.choices.len(), BUNDLED.len() + 31);
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|error| error.contains("limited"))
        );
    }
}
