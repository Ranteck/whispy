//! Tray settings read from the daemon's configuration file.

use std::{
    fmt, fs, io,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Dictation languages offered by the tray.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    /// Spanish dictation.
    Es,
    /// English dictation.
    En,
    /// Detect the spoken language automatically.
    #[default]
    Auto,
}

impl Language {
    /// Value written to `stt.language`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Es => "es",
            Self::En => "en",
            Self::Auto => "auto",
        }
    }

    /// Human-readable name shown in the language submenu.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Es => "Spanish",
            Self::En => "English",
            Self::Auto => "Automatic",
        }
    }

    /// Label of a selectable language menu item.
    pub const fn item_label(self) -> &'static str {
        match self {
            Self::Es => "Spanish (es)",
            Self::En => "English (en)",
            Self::Auto => "Automatic (auto)",
        }
    }
}

/// Settings consumed by the tray independently of the daemon.
#[derive(Debug, PartialEq, Eq)]
pub struct Settings {
    /// Selected dictation language.
    pub language: Language,
    /// Silence playback while recording.
    pub mute_playback: bool,
    /// Copy the latest accepted transcription automatically.
    pub save_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::Auto,
            mute_playback: false,
            save_clipboard: true,
        }
    }
}

fn parse_settings(text: &str) -> Result<Settings, toml::de::Error> {
    let value: toml::Value = toml::from_str(text)?;
    Ok(Settings {
        language: match value
            .get("stt")
            .and_then(|v| v.get("language"))
            .and_then(toml::Value::as_str)
        {
            Some("es") => Language::Es,
            Some("en") => Language::En,
            _ => Language::Auto,
        },
        mute_playback: value
            .get("audio")
            .and_then(|v| v.get("mute_playback"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        save_clipboard: value
            .get("injection")
            .and_then(|v| v.get("save_clipboard"))
            .and_then(toml::Value::as_bool)
            .or_else(|| {
                value
                    .get("audio")
                    .and_then(|v| v.get("save_clipboard"))
                    .and_then(toml::Value::as_bool)
            })
            .unwrap_or(true),
    })
}

/// Load tray settings, logging read or parse failures before using defaults.
pub fn load(path: &Path) -> Settings {
    match fs::read_to_string(path) {
        Ok(text) => {
            if let Ok(settings) = parse_settings(&text) {
                tracing::debug!(event = "tray_settings_loaded", outcome = "loaded");
                settings
            } else {
                // TOML errors include source snippets, so keep configuration out of logs.
                tracing::warn!(
                    event = "tray_settings_loaded",
                    outcome = "invalid_toml",
                    "invalid tray configuration; using defaults"
                );
                Settings::default()
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(event = "tray_settings_loaded", outcome = "missing");
            Settings::default()
        }
        Err(error) => {
            tracing::warn!(event = "tray_settings_loaded", outcome = "read_failed", %error, "could not read tray configuration; using defaults");
            Settings::default()
        }
    }
}

/// Failure to update a configuration without changing unrelated settings.
#[derive(Debug)]
pub enum SettingsError {
    /// The configuration is not valid TOML.
    InvalidToml(toml::de::Error),
    /// The document cannot be edited safely by the line-based editor.
    UnsupportedLayout,
    /// Reading or replacing the configuration failed.
    Io(io::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToml(_) => f.write_str("invalid TOML configuration"),
            Self::UnsupportedLayout => f.write_str("could not safely update stt.language"),
            Self::Io(error) => write!(f, "could not update tray configuration: {error}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidToml(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::UnsupportedLayout => None,
        }
    }
}

/// Change only `stt.language`, retaining surrounding lines and inline comments.
pub fn with_language(text: &str, lang: Language) -> Result<String, SettingsError> {
    let mut expected: toml::Table = toml::from_str(text).map_err(SettingsError::InvalidToml)?;
    let Some(stt) = expected
        .entry("stt")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
    else {
        return Err(SettingsError::UnsupportedLayout);
    };
    stt.insert("language".into(), toml::Value::String(lang.code().into()));
    let mut output = String::new();
    let mut in_stt = false;
    let mut found_stt = false;
    let mut wrote_language = false;
    let assignment = format!("language = \"{}\"\n", lang.code());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let trimmed = body.split('#').next().unwrap_or_default().trim();
        if trimmed.starts_with('[') {
            if in_stt && !wrote_language {
                output.push_str(&assignment);
                wrote_language = true;
            }
            in_stt = trimmed == "[stt]";
            found_stt |= in_stt;
        }
        if in_stt
            && let Some((key, value)) = body.split_once('=')
            && key.trim() == "language"
        {
            let whitespace = &value[..value.len() - value.trim_start().len()];
            let comment = comment_start(value).map_or_else(
                || value.trim_end().len(),
                |index| value[..index].trim_end().len(),
            );
            output.extend([
                key,
                "=",
                whitespace,
                "\"",
                lang.code(),
                "\"",
                &value[comment..],
                ending,
            ]);
            wrote_language = true;
        } else {
            output.push_str(line);
        }
    }
    if !wrote_language {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        if !found_stt {
            output.push_str("[stt]\n");
        }
        output.push_str(&assignment);
    }
    let result: toml::Table = toml::from_str(&output).map_err(SettingsError::InvalidToml)?;
    // Also catch apparent assignments inside multiline strings before writing anything.
    if result != expected {
        return Err(SettingsError::UnsupportedLayout);
    }
    Ok(output)
}

/// Persist a language selection, or return false if the configuration is absent.
pub fn write_language(path: &Path, lang: Language) -> Result<bool, SettingsError> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(event = "tray_language_updated", outcome = "missing");
            return Ok(false);
        }
        Err(error) => return Err(SettingsError::Io(error)),
    };
    let text = fs::read_to_string(&target).map_err(SettingsError::Io)?;
    let updated = with_language(&text, lang)?;
    let permissions = fs::metadata(&target)
        .map_err(SettingsError::Io)?
        .permissions();
    let mut temporary = target.as_os_str().to_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    // Recover the fixed temporary path after an interrupted previous write.
    if let Err(error) = fs::remove_file(&temporary)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(SettingsError::Io(error));
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(permissions.mode())
        .open(&temporary)
        .map_err(SettingsError::Io)?;
    file.write_all(updated.as_bytes())
        .map_err(SettingsError::Io)?;
    // Creation applies the process umask; restore the user's exact mode before replacement.
    file.set_permissions(permissions)
        .map_err(SettingsError::Io)?;
    file.sync_all().map_err(SettingsError::Io)?;
    fs::rename(&temporary, &target).map_err(SettingsError::Io)?;
    tracing::info!(
        event = "tray_language_updated",
        outcome = "updated",
        language = lang.code()
    );
    Ok(true)
}

fn comment_start(value: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if ch == '\\' => escaped = true,
            Some(delimiter) if ch == delimiter => quote = None,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return Some(index),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Settings {
        parse_settings(text).unwrap_or_default()
    }

    #[test]
    fn parse_defaults_language_auto() {
        let s = parse("");
        assert_eq!(s.language, Language::Auto);
        assert!(!s.mute_playback);
        assert!(s.save_clipboard);
    }

    #[test]
    fn save_clipboard_defaults_and_fallback() {
        assert!(!parse("[audio]\nsave_clipboard = false").save_clipboard);
        assert!(
            parse("[injection]\nsave_clipboard = true\n[audio]\nsave_clipboard = false")
                .save_clipboard
        );
        assert!(!parse("[injection]\nsave_clipboard = false").save_clipboard);
    }

    #[test]
    fn parse_reads_language_and_mute_with_typed_defaults() {
        for (code, language) in [
            ("es", Language::Es),
            ("en", Language::En),
            ("auto", Language::Auto),
        ] {
            let s = parse(&format!(
                "[stt]\nlanguage = \"{code}\"\n[audio]\nmute_playback = true"
            ));
            assert_eq!(s.language, language);
            assert!(s.mute_playback);
        }
        assert_eq!(
            parse("[stt]\nlanguage = \"fr\"\n[audio]\nmute_playback = \"true\""),
            Settings::default()
        );
        assert_eq!(parse("[broken"), Settings::default());
    }

    #[test]
    fn language_labels_match_the_menu() {
        for (language, code, name, label) in [
            (Language::Es, "es", "Spanish", "Spanish (es)"),
            (Language::En, "en", "English", "English (en)"),
            (Language::Auto, "auto", "Automatic", "Automatic (auto)"),
        ] {
            assert_eq!(language.code(), code);
            assert_eq!(language.name(), name);
            assert_eq!(language.item_label(), label);
        }
    }

    #[test]
    fn with_language_replaces_only_stt_language() {
        let src = "[stt]\nlanguage = \"es\" # mine\n\n[workflows.x]\nlanguage = \"fr\"\n";
        assert_eq!(
            with_language(src, Language::En).unwrap(),
            "[stt]\nlanguage = \"en\" # mine\n\n[workflows.x]\nlanguage = \"fr\"\n"
        );
    }

    #[test]
    fn with_language_adds_key_or_table() {
        for (src, expected) in [
            (
                "[stt]\nmodel = \"m\"\n",
                "[stt]\nmodel = \"m\"\nlanguage = \"auto\"\n",
            ),
            ("", "[stt]\nlanguage = \"auto\"\n"),
            (
                "[audio]\nmute_playback = true",
                "[audio]\nmute_playback = true\n[stt]\nlanguage = \"auto\"\n",
            ),
            (
                "[stt] # voice\nmodel = \"m\"\n[audio]\nmute_playback = true\n",
                "[stt] # voice\nmodel = \"m\"\nlanguage = \"auto\"\n[audio]\nmute_playback = true\n",
            ),
        ] {
            assert_eq!(with_language(src, Language::Auto).unwrap(), expected);
        }
    }

    #[test]
    fn with_language_rejects_edits_to_multiline_text() {
        let src = "[stt]\nlanguage = \"es\"\nnotes = '''\nlanguage = \"fr\"\n'''\n";
        assert!(matches!(
            with_language(src, Language::En),
            Err(SettingsError::UnsupportedLayout)
        ));
    }

    #[test]
    fn with_language_preserves_whitespace_comments_and_line_endings() {
        let src = "# header\r\n[stt]\r\n  language  =  'es#tag'   # keep\r\n[workflow.x]\r\nlanguage = 'fr'\r\n";
        assert_eq!(
            with_language(src, Language::En).unwrap(),
            "# header\r\n[stt]\r\n  language  =  \"en\"   # keep\r\n[workflow.x]\r\nlanguage = 'fr'\r\n"
        );
    }

    #[test]
    fn load_reads_settings_and_defaults_on_missing_or_invalid_files() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-load-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_writer(io::stderr)
                .finish(),
        );
        assert_eq!(load(&dir.join("missing.toml")), Settings::default());
        fs::write(
            &path,
            "[stt]\nlanguage = 'es'\n[audio]\nmute_playback = true\nsave_clipboard = false\n",
        )
        .unwrap();
        assert_eq!(
            load(&path),
            Settings {
                language: Language::Es,
                mute_playback: true,
                save_clipboard: false
            }
        );
        fs::write(&path, "[broken").unwrap();
        assert_eq!(load(&path), Settings::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_language_missing_file_is_noop() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-missing-{}", std::process::id()));
        assert!(!write_language(&dir.join("nope.toml"), Language::Es).unwrap());
        assert!(!dir.exists());
    }

    #[test]
    fn write_language_atomically_replaces_existing_config() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(
            &path,
            "[stt]\nlanguage = 'es' # keep\n[workflow.x]\nlanguage = 'fr'\n",
        )
        .unwrap();
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_writer(io::stderr)
                .finish(),
        );
        assert!(write_language(&path, Language::En).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[stt]\nlanguage = \"en\" # keep\n[workflow.x]\nlanguage = 'fr'\n"
        );
        assert!(!dir.join("config.toml.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_language_follows_symlink() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-symlink-{}", std::process::id()));
        let config_dir = dir.join("config");
        let target_dir = dir.join("dotfiles");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        let path = config_dir.join("config.toml");
        let target = target_dir.join("config.toml");
        fs::write(&target, "[stt]\nlanguage = 'es'\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(write_language(&path, Language::En).unwrap());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&path).unwrap(), target);
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "[stt]\nlanguage = \"en\"\n"
        );
        assert!(!target_dir.join("config.toml.tmp").exists());
        assert!(!config_dir.join("config.toml.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_language_preserves_mode() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-mode-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        for mode in [0o644, 0o600] {
            fs::write(&path, "[stt]\nlanguage = 'es'\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(write_language(&path, Language::En).unwrap());
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                mode
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "[stt]\nlanguage = \"en\"\n"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_language_replaces_stale_temp() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-stale-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for filename in ["config.toml", "settings.conf"] {
            let path = dir.join(filename);
            let temporary = dir.join(format!("{filename}.tmp"));
            fs::write(&path, "[stt]\nlanguage = 'es'\n").unwrap();
            fs::write(&temporary, "partial previous write").unwrap();

            assert!(write_language(&path, Language::En).unwrap());
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "[stt]\nlanguage = \"en\"\n"
            );
            assert!(!temporary.exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_language_preserves_config_on_invalid_toml_or_io_error() {
        let dir = std::env::temp_dir().join(format!("whispy-tray-errors-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "[broken").unwrap();
        let error = write_language(&path, Language::Es).unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[broken");
        assert!(!dir.join("config.toml.tmp").exists());

        fs::write(&path, "[stt]\nlanguage = 'es'\n").unwrap();
        fs::create_dir(dir.join("config.toml.tmp")).unwrap();
        let error = write_language(&path, Language::En).unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[stt]\nlanguage = 'es'\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
