//! Subprocess, clipboard, and audio sink effects for the tray.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use tracing::{debug, warn};

/// Resolve the `whispy-client` binary next to `exe`, falling back to `PATH`.
pub fn client_path(exe: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        let sibling = dir.join("whispy-client");
        if exists(&sibling) {
            return sibling;
        }
    }
    PathBuf::from("whispy-client")
}

/// Spawn `program` with `args` on a background thread and log a warning on failure.
pub fn spawn_detached(program: &Path, args: &[&str]) {
    let program = program.to_path_buf();
    let args: Vec<String> = args.iter().copied().map(String::from).collect();
    std::thread::spawn(move || {
        let status = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(status) if status.success() => {
                debug!(program = %program.display(), ?args, "detached command finished");
            }
            Ok(status) => {
                warn!(
                    program = %program.display(),
                    ?args,
                    %status,
                    "detached command exited with error"
                );
            }
            Err(err) => {
                warn!(
                    program = %program.display(),
                    ?args,
                    error = %err,
                    "failed to spawn detached command"
                );
            }
        }
    });
}

/// Copy `text` to the Wayland clipboard via `wl-copy` stdin.
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let write_res = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Err(io::Error::other("wl-copy stdin unavailable")),
    };
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("wl-copy exited with {status}")));
    }
    write_res?;
    debug!(bytes = text.len(), "copied text to clipboard");
    Ok(())
}

/// Check whether `wpctl get-volume` output indicates a muted sink.
pub fn wpctl_muted(stdout: &str) -> bool {
    stdout.contains("[MUTED]")
}

/// Check whether `pactl get-sink-mute` output indicates a muted sink.
pub fn pactl_muted(stdout: &str) -> bool {
    stdout.contains("Mute: yes")
}

enum MuteRequest {
    Set(bool),
    Restore(mpsc::Sender<()>),
}

/// Sequential worker that mutes and restores the default audio sink.
#[derive(Debug)]
pub struct Muter {
    tx: mpsc::Sender<MuteRequest>,
}

impl Muter {
    /// Start the background audio muter worker thread.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut muted_by_tray = false;
            for req in rx {
                match req {
                    MuteRequest::Set(true) => mute_sink(&mut muted_by_tray),
                    MuteRequest::Set(false) => unmute_sink(&mut muted_by_tray),
                    MuteRequest::Restore(ack) => {
                        unmute_sink(&mut muted_by_tray);
                        // The sink is already restored; nothing more to do if the caller dropped ack_rx.
                        let _ = ack.send(());
                    }
                }
            }
        });
        Self { tx }
    }

    /// Queue a mute or unmute request.
    pub fn set(&self, mute: bool) {
        if self.tx.send(MuteRequest::Set(mute)).is_err() {
            warn!(mute, "audio muter worker thread is gone");
        }
    }

    /// Restore the sink if muted by the tray and wait for completion.
    pub fn restore(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(MuteRequest::Restore(ack_tx)).is_err() {
            warn!("audio muter worker thread is gone; cannot restore audio sink");
            return;
        }
        if ack_rx.recv().is_err() {
            warn!("audio muter worker thread exited before confirming restore");
        }
    }
}

fn mute_sink(muted_by_tray: &mut bool) {
    if let Some(stdout) = capture_stdout("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        if wpctl_muted(&stdout) {
            *muted_by_tray = false;
            debug!(tool = "wpctl", "audio sink already muted");
            return;
        }
        if run_quiet("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "1"]) {
            *muted_by_tray = true;
            debug!(tool = "wpctl", "muted audio sink");
            return;
        }
    }
    if let Some(stdout) = capture_stdout("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]) {
        if pactl_muted(&stdout) {
            *muted_by_tray = false;
            debug!(tool = "pactl", "audio sink already muted");
            return;
        }
        if run_quiet("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "1"]) {
            *muted_by_tray = true;
            debug!(tool = "pactl", "muted audio sink");
            return;
        }
    }
    warn!("failed to mute audio sink: neither wpctl nor pactl succeeded");
}

fn unmute_sink(muted_by_tray: &mut bool) {
    if !*muted_by_tray {
        return;
    }
    *muted_by_tray = false;
    if run_quiet("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]) {
        debug!(tool = "wpctl", "restored audio sink");
        return;
    }
    if run_quiet("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"]) {
        debug!(tool = "pactl", "restored audio sink");
        return;
    }
    warn!("failed to restore audio sink: neither wpctl nor pactl succeeded");
}

fn capture_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_prefers_sibling_binary() {
        let exe = Path::new("/opt/whispy/bin/whispy-tray");
        let sibling = Path::new("/opt/whispy/bin/whispy-client");
        assert_eq!(client_path(exe, |p| p == sibling), sibling);
        assert_eq!(client_path(exe, |_| false), Path::new("whispy-client"));
        assert_eq!(
            client_path(Path::new("whispy-tray"), |_| true),
            Path::new("whispy-client")
        );
    }

    #[test]
    fn wpctl_muted_parses_marker() {
        assert!(wpctl_muted("Volume: 1.00 [MUTED]\n"));
        assert!(wpctl_muted("Volume: 1.00 [MUTED]"));
        assert!(!wpctl_muted("Volume: 1.00\n"));
        assert!(!wpctl_muted("Volume: 1.00"));
    }

    #[test]
    fn pactl_muted_parses_yes() {
        assert!(pactl_muted("Mute: yes\n"));
        assert!(pactl_muted("Mute: yes"));
        assert!(!pactl_muted("Mute: no\n"));
        assert!(!pactl_muted("Mute: no"));
    }
}
