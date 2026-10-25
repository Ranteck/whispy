//! Presentation states used by the tray.

use std::path::Path;

use whispy_common::{State, StateSnapshot};

/// Visual state of the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ready to dictate.
    Idle,
    /// Capturing audio.
    Recording,
    /// Converting captured audio to text.
    Transcribing,
    /// Dictation failed.
    Error,
}

/// Read a daemon snapshot, returning no state when it is unavailable or invalid.
pub fn read_snapshot(path: &Path) -> Option<StateSnapshot> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(error_kind = ?error.kind(), "daemon state unavailable; showing idle");
            return None;
        }
    };
    match serde_json::from_slice::<StateSnapshot>(&bytes) {
        Ok(snapshot) => {
            tracing::debug!(state = ?snapshot.state, "read daemon state snapshot");
            Some(snapshot)
        }
        Err(error) => {
            tracing::debug!(error_kind = ?error.classify(), "invalid daemon state; showing idle");
            None
        }
    }
}

/// Return the lifecycle state and error text that affect the tray.
pub fn key(snapshot: Option<&StateSnapshot>) -> (State, Option<String>) {
    snapshot.map_or((State::Idle, None), |snapshot| {
        (snapshot.state, snapshot.error_message.clone())
    })
}

/// Map the daemon lifecycle state to its tray appearance.
pub fn kind(state: State) -> Kind {
    match state {
        State::Recording => Kind::Recording,
        State::Transcribing => Kind::Transcribing,
        State::Error => Kind::Error,
        State::Idle | State::Success => Kind::Idle,
    }
}

/// Build the tooltip for the current tray appearance.
pub fn tooltip(kind: Kind, error: Option<&str>) -> String {
    match kind {
        Kind::Idle => "Whispy: Ready".to_owned(),
        Kind::Recording => "Whispy: Listening...".to_owned(),
        Kind::Transcribing => "Whispy: Transcribing...".to_owned(),
        Kind::Error => error.map_or_else(
            || "Whispy: Error".to_owned(),
            |message| format!("Whispy: Error ({message})"),
        ),
    }
}

/// Return the disabled menu heading for the current tray appearance.
pub fn header(kind: Kind) -> &'static str {
    match kind {
        Kind::Idle => "Whispy: ✓ Ready to dictate",
        Kind::Recording => "Whispy: ● Recording audio...",
        Kind::Transcribing => "Whispy: ⏳ Transcribing...",
        Kind::Error => "Whispy: ⚠️ Status: Error",
    }
}

/// Return the dictation action label for the current tray appearance.
pub fn toggle_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Recording => "Stop and transcribe",
        Kind::Idle | Kind::Transcribing | Kind::Error => "Start dictation",
    }
}

/// System actions requested by a daemon state transition.
#[derive(Debug)]
pub struct Effects {
    /// Mute on entering recording and restore on leaving it.
    pub mute: Option<bool>,
    /// Schedule copying the newest accepted transcript.
    pub auto_copy: bool,
}

/// Decide which system actions a lifecycle transition requests.
pub fn effects(old: State, new: State) -> Effects {
    let was_recording = old == State::Recording;
    let is_recording = new == State::Recording;
    Effects {
        mute: (was_recording != is_recording).then_some(is_recording),
        auto_copy: old != new
            && matches!(old, State::Recording | State::Transcribing | State::Success)
            && matches!(new, State::Success | State::Idle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whispy_common::State;

    #[test]
    fn tooltip_per_state() {
        assert_eq!(tooltip(Kind::Idle, None), "Whispy: Ready");
        assert_eq!(tooltip(Kind::Recording, None), "Whispy: Listening...");
        assert_eq!(tooltip(Kind::Transcribing, None), "Whispy: Transcribing...");
        assert_eq!(tooltip(Kind::Error, Some("boom")), "Whispy: Error (boom)");
        assert_eq!(tooltip(Kind::Error, None), "Whispy: Error");
        assert_eq!(kind(State::Idle), Kind::Idle);
        assert_eq!(kind(State::Recording), Kind::Recording);
        assert_eq!(kind(State::Transcribing), Kind::Transcribing);
        assert_eq!(kind(State::Success), Kind::Idle);
        assert_eq!(kind(State::Error), Kind::Error);
        assert_eq!(header(Kind::Idle), "Whispy: ✓ Ready to dictate");
        assert_eq!(header(Kind::Recording), "Whispy: ● Recording audio...");
        assert_eq!(header(Kind::Transcribing), "Whispy: ⏳ Transcribing...");
        assert_eq!(header(Kind::Error), "Whispy: ⚠️ Status: Error");
        assert_eq!(toggle_label(Kind::Recording), "Stop and transcribe");
        for kind in [Kind::Idle, Kind::Transcribing, Kind::Error] {
            assert_eq!(toggle_label(kind), "Start dictation");
        }
    }

    #[test]
    fn missing_or_invalid_state_reads_as_idle() {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_test_writer()
            .without_time()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let dir = std::env::temp_dir().join(format!(
            "whispy-tray-view-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create snapshot test directory");
        let missing = dir.join("missing.json");
        let garbage = dir.join("state.json");
        assert!(read_snapshot(&missing).is_none());
        assert!(read_snapshot(&dir).is_none());
        std::fs::write(&garbage, "not JSON").expect("write malformed snapshot");
        assert!(read_snapshot(&garbage).is_none());
        std::fs::write(&garbage, r#"{"state":"recording","rms":"loud"}"#)
            .expect("write invalid snapshot fields");
        assert!(read_snapshot(&garbage).is_none());
        assert_eq!(key(None), (State::Idle, None));

        std::fs::write(
            &garbage,
            r#"{"state":"recording","rms":0.5,"error_kind":null,"error_message":null,"timestamp":42.0}"#,
        )
        .expect("write valid snapshot");
        let snapshot = read_snapshot(&garbage).expect("valid snapshot was discarded");
        assert_eq!(snapshot.state, State::Recording);
        std::fs::remove_dir_all(dir).expect("remove snapshot test directory");
    }

    #[test]
    fn rms_only_change_keeps_key() {
        let first = StateSnapshot {
            state: State::Recording,
            rms: 0.1,
            timestamp: 10.0,
            ..StateSnapshot::default()
        };
        let second = StateSnapshot {
            rms: 0.9,
            timestamp: 11.0,
            ..first.clone()
        };
        assert_eq!(key(Some(&first)), (State::Recording, None));
        assert_eq!(key(Some(&first)), key(Some(&second)));

        let error = StateSnapshot {
            state: State::Error,
            error_kind: Some("stt_error".to_owned()),
            error_message: Some("boom".to_owned()),
            ..second
        };
        assert_eq!(key(Some(&error)), (State::Error, Some("boom".to_owned())));
        let changed_message = StateSnapshot {
            error_message: Some("another error".to_owned()),
            ..error.clone()
        };
        assert_ne!(key(Some(&error)), key(Some(&changed_message)));
        let changed_error_kind = StateSnapshot {
            error_kind: Some("too_long".to_owned()),
            ..error.clone()
        };
        assert_eq!(key(Some(&error)), key(Some(&changed_error_kind)));
    }

    #[test]
    fn mute_on_enter_and_unmute_on_leave_recording() {
        assert_eq!(effects(State::Idle, State::Recording).mute, Some(true));
        assert!(!effects(State::Idle, State::Recording).auto_copy);
        assert_eq!(
            effects(State::Recording, State::Transcribing).mute,
            Some(false)
        );
        assert_eq!(effects(State::Transcribing, State::Success).mute, None);
        assert_eq!(effects(State::Recording, State::Recording).mute, None);
        for state in [State::Idle, State::Success, State::Error] {
            assert_eq!(effects(State::Recording, state).mute, Some(false));
            assert_eq!(effects(state, State::Recording).mute, Some(true));
        }
    }

    #[test]
    fn auto_copy_after_dictation() {
        assert!(effects(State::Transcribing, State::Success).auto_copy);
        assert!(effects(State::Success, State::Idle).auto_copy);
        assert!(!effects(State::Idle, State::Recording).auto_copy);
        assert!(!effects(State::Error, State::Idle).auto_copy);
        for old in [State::Recording, State::Transcribing, State::Success] {
            for new in [State::Success, State::Idle] {
                assert_eq!(
                    effects(old, new).auto_copy,
                    old != new,
                    "{old:?} -> {new:?}"
                );
            }
        }
        for old in [State::Idle, State::Error] {
            for new in [State::Idle, State::Success] {
                assert!(!effects(old, new).auto_copy, "{old:?} -> {new:?}");
            }
        }
        for old in [
            State::Idle,
            State::Recording,
            State::Transcribing,
            State::Success,
            State::Error,
        ] {
            for new in [State::Recording, State::Transcribing, State::Error] {
                assert!(!effects(old, new).auto_copy, "{old:?} -> {new:?}");
            }
        }
    }
}
