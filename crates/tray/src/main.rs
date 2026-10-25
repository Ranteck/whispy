//! System tray indicator for the whispy dictation daemon.

mod history;
mod icons;
mod menu;
mod paths;
mod settings;
mod system;
mod view;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ldtray::{Event, Notification, Tray, TrayConfig, TrayHandle};
use tracing::{debug, error, info, warn};
use whispy_common::State;

use crate::history::Transcript;
use crate::menu::Action;
use crate::paths::Paths;
use crate::settings::Language;
use crate::system::Muter;
use crate::view::Kind;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const AUTO_COPY_DELAY: Duration = Duration::from_millis(100);
const HISTORY_LIMIT: usize = 10;
const COPY_PREVIEW_MAX_CHARS: usize = 55;

fn main() -> ExitCode {
    init_logging();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "whispy-tray failed");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::from_env(|key| std::env::var(key).ok());
    let exe = std::env::current_exe().unwrap_or_default();
    let client = system::client_path(&exe, Path::exists);
    let mut last_copied = history::recent(&paths.transcripts, 1)
        .first()
        .map_or(0.0, |entry| entry.timestamp);
    let mut history = history::recent(&paths.transcripts, HISTORY_LIMIT);
    let initial_settings = settings::load(&paths.config);
    let mut current_key = (State::Idle, None);
    let initial_kind = view::kind(current_key.0);

    let config = TrayConfig::new(icons::icon(initial_kind))
        .tooltip(view::tooltip(initial_kind, None))
        .menu(menu::build(
            initial_kind,
            &history,
            initial_settings.language,
        ));

    let (event_tx, event_rx) = mpsc::channel();
    let handle = Tray::new(config)?.spawn(move |event| {
        // The receiver only drops when the main event loop is shutting down.
        let _ = event_tx.send(event);
    })?;

    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    let muter = Muter::spawn();
    let mut pending_copy: Option<Instant> = None;

    info!(
        state = %paths.state.display(),
        config = %paths.config.display(),
        transcripts = %paths.transcripts.display(),
        "started whispy-tray"
    );

    let mut quit_by_menu = false;
    while !shutdown.load(Ordering::Relaxed) {
        match event_rx.recv_timeout(POLL_INTERVAL) {
            Ok(event) => {
                if handle_event(event, &paths, &client, &handle, current_key.0, &mut history) {
                    quit_by_menu = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                poll_state(
                    &paths,
                    &handle,
                    &muter,
                    &mut current_key,
                    &mut history,
                    &mut pending_copy,
                );
                if pending_copy.is_some_and(|deadline| Instant::now() >= deadline) {
                    pending_copy = None;
                    run_auto_copy(&paths.transcripts, &mut last_copied);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                muter.restore();
                return Err("tray event channel disconnected".into());
            }
        }
    }

    if !quit_by_menu {
        info!(
            action = "signal_shutdown",
            "received shutdown signal; quitting whispy-tray"
        );
    }
    muter.restore();
    if let Err(error) = handle.quit() {
        warn!(%error, "failed to stop tray backend");
    }
    Ok(())
}

fn handle_event(
    event: Event,
    paths: &Paths,
    client: &Path,
    handle: &TrayHandle,
    current_state: State,
    history: &mut Vec<Transcript>,
) -> bool {
    match event {
        Event::LeftClick => {
            info!(
                action = "toggle",
                trigger = "left_click",
                "toggling dictation"
            );
            system::spawn_detached(client, &["toggle"]);
            false
        }
        Event::MiddleClick => {
            info!(
                action = "cancel",
                trigger = "middle_click",
                "cancelling dictation"
            );
            system::spawn_detached(client, &["cancel"]);
            false
        }
        Event::Menu(id) => {
            if let Some(action) = menu::action(id) {
                handle_menu_action(action, paths, client, handle, current_state, history)
            } else {
                debug!(menu_id = id.0, "ignoring unmapped menu item");
                false
            }
        }
        other => {
            debug!(?other, "ignoring unhandled tray event");
            false
        }
    }
}

fn handle_menu_action(
    action: Action,
    paths: &Paths,
    client: &Path,
    handle: &TrayHandle,
    current_state: State,
    history: &mut Vec<Transcript>,
) -> bool {
    match action {
        Action::Toggle => {
            info!(action = "toggle", trigger = "menu", "toggling dictation");
            system::spawn_detached(client, &["toggle"]);
            false
        }
        Action::Cancel => {
            info!(action = "cancel", trigger = "menu", "cancelling dictation");
            system::spawn_detached(client, &["cancel"]);
            send_notification(
                handle,
                "Dictation cancelled",
                "Audio capture was discarded",
                Kind::Error,
            );
            false
        }
        Action::Copy(index) => {
            copy_history_entry(handle, history, index);
            false
        }
        Action::OpenLog => {
            info!(
                action = "open_log",
                path = %paths.transcripts.display(),
                "opening transcript log"
            );
            let target = paths.transcripts.to_string_lossy();
            system::spawn_detached(Path::new("xdg-open"), &[&target]);
            false
        }
        Action::EditConfig => {
            info!(
                action = "edit_config",
                path = %paths.config.display(),
                "opening configuration"
            );
            let target = paths.config.to_string_lossy();
            system::spawn_detached(Path::new("xdg-open"), &[&target]);
            false
        }
        Action::SetLanguage(language) => {
            apply_language_change(paths, handle, current_state, history, language);
            false
        }
        Action::RestartDaemon => {
            info!(
                action = "restart_daemon",
                "restarting whispy-daemon.service"
            );
            system::spawn_detached(
                Path::new("systemctl"),
                &["--user", "restart", "whispy-daemon.service"],
            );
            send_notification(handle, "Whispy", "Restarting whispy-daemon...", Kind::Idle);
            false
        }
        Action::Quit => {
            info!(action = "quit", "quitting whispy-tray");
            true
        }
    }
}

fn copy_history_entry(handle: &TrayHandle, history: &[Transcript], index: usize) {
    let Some(text) = transcript_to_copy(history, index) else {
        warn!(
            action = "copy_history",
            index, "selected transcript is unavailable"
        );
        return;
    };
    match system::copy_to_clipboard(text) {
        Ok(()) => {
            info!(
                action = "copy_history",
                index, "copied transcript from history"
            );
            send_notification(
                handle,
                "Copied to clipboard",
                history::preview(text, COPY_PREVIEW_MAX_CHARS),
                Kind::Idle,
            );
        }
        Err(error) => {
            warn!(
                action = "copy_history",
                index,
                %error,
                "failed to copy transcript to clipboard"
            );
        }
    }
}

fn apply_language_change(
    paths: &Paths,
    handle: &TrayHandle,
    current_state: State,
    history: &mut Vec<Transcript>,
    language: Language,
) {
    match settings::write_language(&paths.config, language) {
        Ok(true) => {
            system::spawn_detached(
                Path::new("systemctl"),
                &["--user", "restart", "whispy-daemon.service"],
            );
            send_notification(
                handle,
                "Dictation Language",
                language_switched_body(language),
                Kind::Idle,
            );
            *history = history::recent(&paths.transcripts, HISTORY_LIMIT);
            let menu = menu::build(view::kind(current_state), history, language);
            if let Err(error) = handle.set_menu(menu) {
                warn!(%error, "failed to update tray menu after language change");
            }
        }
        Ok(false) => {
            info!(
                action = "set_language",
                language = language.code(),
                outcome = "missing_config",
                "configuration file missing; language not updated"
            );
        }
        Err(error) => {
            warn!(
                action = "set_language",
                language = language.code(),
                %error,
                "failed to update configuration language"
            );
        }
    }
}

fn poll_state(
    paths: &Paths,
    handle: &TrayHandle,
    muter: &Muter,
    current_key: &mut (State, Option<String>),
    history: &mut Vec<Transcript>,
    pending_copy: &mut Option<Instant>,
) {
    let snapshot = view::read_snapshot(&paths.state);
    let new_key = view::key(snapshot.as_ref());
    if new_key == *current_key {
        return;
    }

    let old_state = current_key.0;
    *current_key = new_key;
    let (new_state, ref error_message) = *current_key;
    let kind = view::kind(new_state);

    if let Err(error) = handle.set_icon(icons::icon(kind)) {
        warn!(%error, ?kind, "failed to update tray icon");
    }
    if let Err(error) = handle.set_tooltip(view::tooltip(kind, error_message.as_deref())) {
        warn!(%error, ?kind, "failed to update tray tooltip");
    }

    *history = history::recent(&paths.transcripts, HISTORY_LIMIT);
    let settings = settings::load(&paths.config);
    if let Err(error) = handle.set_menu(menu::build(kind, history, settings.language)) {
        warn!(%error, ?kind, "failed to update tray menu");
    }

    let fx = view::effects(old_state, new_state);
    match fx.mute {
        Some(true) if settings.mute_playback => muter.set(true),
        Some(false) => muter.set(false),
        _ => {}
    }
    if fx.auto_copy && settings.save_clipboard {
        *pending_copy = Some(Instant::now() + AUTO_COPY_DELAY);
    }
}

fn run_auto_copy(transcripts_path: &Path, last_copied: &mut f64) {
    let recent = history::recent(transcripts_path, 1);
    if let Some((text, timestamp)) = next_auto_copy(&recent, *last_copied) {
        *last_copied = timestamp;
        if let Err(error) = system::copy_to_clipboard(text) {
            warn!(%error, timestamp, "failed to auto-copy latest transcript to clipboard");
        } else {
            debug!(timestamp, "auto-copied latest transcript to clipboard");
        }
    }
}

fn send_notification(handle: &TrayHandle, title: &str, body: impl Into<String>, kind: Kind) {
    let notification = Notification::new(title, body).with_icon(icons::icon(kind));
    if let Err(error) = handle.notify(notification) {
        warn!(%error, title, "failed to send desktop notification");
    }
}

fn next_auto_copy(recent: &[Transcript], last_copied: f64) -> Option<(&str, f64)> {
    let latest = recent.first()?;
    let trimmed = latest.text.trim();
    (latest.timestamp > last_copied && !trimmed.is_empty()).then_some((trimmed, latest.timestamp))
}

fn transcript_to_copy(history: &[Transcript], index: usize) -> Option<&str> {
    let text = history.get(index)?.text.trim();
    (!text.is_empty()).then_some(text)
}

fn language_switched_body(lang: Language) -> String {
    format!("Switched to: {}", lang.item_label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_auto_copy_trims_and_requires_newer_timestamp() {
        assert_eq!(next_auto_copy(&[], 0.0), None);

        let transcripts = [Transcript {
            text: "  hello world \n".to_owned(),
            timestamp: 10.5,
        }];
        assert_eq!(next_auto_copy(&transcripts, 10.5), None);
        assert_eq!(next_auto_copy(&transcripts, 11.0), None);
        assert_eq!(
            next_auto_copy(&transcripts, 10.0),
            Some(("hello world", 10.5))
        );

        let blank = [Transcript {
            text: "   \n\t ".to_owned(),
            timestamp: 20.0,
        }];
        assert_eq!(next_auto_copy(&blank, 10.0), None);
    }

    #[test]
    fn transcript_to_copy_trims_selected_entry() {
        let history = [
            Transcript {
                text: "  first entry\n".to_owned(),
                timestamp: 2.0,
            },
            Transcript {
                text: "\tsecond entry  ".to_owned(),
                timestamp: 1.0,
            },
            Transcript {
                text: "   ".to_owned(),
                timestamp: 0.5,
            },
        ];
        assert_eq!(transcript_to_copy(&history, 0), Some("first entry"));
        assert_eq!(transcript_to_copy(&history, 1), Some("second entry"));
        assert_eq!(transcript_to_copy(&history, 2), None);
        assert_eq!(transcript_to_copy(&history, 3), None);
    }

    #[test]
    fn language_switched_body_matches_spec() {
        assert_eq!(
            language_switched_body(Language::Es),
            "Switched to: Spanish (es)"
        );
        assert_eq!(
            language_switched_body(Language::En),
            "Switched to: English (en)"
        );
        assert_eq!(
            language_switched_body(Language::Auto),
            "Switched to: Automatic (auto)"
        );
    }
}
