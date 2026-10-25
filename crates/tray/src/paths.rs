//! Configuration and state paths shared with the daemon's conventions.

use std::path::PathBuf;

/// Files consumed by the tray.
#[derive(Debug)]
pub struct Paths {
    /// Daemon state snapshot.
    pub state: PathBuf,
    /// User configuration.
    pub config: PathBuf,
    /// Accepted and rejected STT records.
    pub transcripts: PathBuf,
}

impl Paths {
    /// Resolve XDG paths using the supplied environment reader.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let runtime = var("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let config = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let state = var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Self {
            state: runtime.join("whispy/state.json"),
            config: config.join("whispy/config.toml"),
            transcripts: state.join("whispy/transcripts.jsonl"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_prefer_xdg_directories() {
        let paths = Paths::from_env(|key| match key {
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".into()),
            "XDG_CONFIG_HOME" => Some("/config".into()),
            "XDG_STATE_HOME" => Some("/state".into()),
            "HOME" => Some("/home/example".into()),
            _ => None,
        });
        assert_eq!(
            paths.state,
            PathBuf::from("/run/user/1000/whispy/state.json")
        );
        assert_eq!(paths.config, PathBuf::from("/config/whispy/config.toml"));
        assert_eq!(
            paths.transcripts,
            PathBuf::from("/state/whispy/transcripts.jsonl")
        );
    }

    #[test]
    fn paths_fall_back_to_home_and_tmp() {
        let paths = Paths::from_env(|key| (key == "HOME").then(|| "/home/example".into()));
        assert_eq!(paths.state, PathBuf::from("/tmp/whispy/state.json"));
        assert_eq!(
            paths.config,
            PathBuf::from("/home/example/.config/whispy/config.toml")
        );
        assert_eq!(
            paths.transcripts,
            PathBuf::from("/home/example/.local/state/whispy/transcripts.jsonl")
        );

        let paths = Paths::from_env(|_| None);
        assert_eq!(paths.state, PathBuf::from("/tmp/whispy/state.json"));
        assert_eq!(paths.config, PathBuf::from("/tmp/whispy/config.toml"));
        assert_eq!(
            paths.transcripts,
            PathBuf::from("/tmp/whispy/transcripts.jsonl")
        );
    }
}
