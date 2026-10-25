//! Subprocess, clipboard, and audio sink effects for the tray.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

const TOOL_TIMEOUT: Duration = Duration::from_secs(2);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_POLL: Duration = Duration::from_millis(5);

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
    let output = run_command(
        Command::new("wl-copy").stdout(Stdio::null()),
        Some(text.as_bytes()),
        TOOL_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "wl-copy exited with {}",
            output.status
        )));
    }
    debug!(bytes = text.len(), "copied text to clipboard");
    Ok(())
}

// The daemon's deadline helpers are private to its binary; keep this local equivalent bounded too.
fn run_command(cmd: &mut Command, input: Option<&[u8]>, timeout: Duration) -> io::Result<Output> {
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let writer = input.map(|input| {
        let stdin = child.stdin.take();
        let input = input.to_vec();
        let (tx, rx) = mpsc::channel();
        // A child that never reads must not block the deadline poll on a full pipe.
        std::thread::spawn(move || {
            let result = stdin
                .ok_or_else(|| io::Error::other("child stdin unavailable"))
                .and_then(|mut stdin| stdin.write_all(&input));
            // The receiver drops when the command times out.
            let _ = tx.send(result);
        });
        rx
    });
    let reader = child.stdout.take().map(|mut stdout| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
            // The receiver drops when the command times out.
            let _ = tx.send(result);
        });
        rx
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(
                    PROCESS_POLL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            result => {
                // Always reap after killing, including when polling itself failed.
                let kill_result = child.kill();
                let reap_result = child.wait();
                result?;
                kill_result?;
                reap_result?;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
            }
        }
    };
    if let Some(writer) = writer {
        receive_until(&writer, deadline)?;
    }
    Ok(Output {
        status,
        stdout: match reader {
            Some(reader) => receive_until(&reader, deadline)?,
            None => Vec::new(),
        },
        stderr: Vec::new(),
    })
}

fn receive_until<T>(rx: &mpsc::Receiver<io::Result<T>>, deadline: Instant) -> io::Result<T> {
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "command pipe timed out",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("command pipe worker exited"))
        }
    }
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

enum MutedSink {
    Wpctl(u32),
    Pactl(String),
}

/// Sequential worker that mutes and restores the default audio sink.
#[derive(Debug)]
pub struct Muter {
    tx: mpsc::Sender<MuteRequest>,
}

impl Muter {
    /// Start the background audio muter worker thread.
    pub fn spawn() -> Self {
        Self::spawn_with_runner(run_audio_command)
    }

    fn spawn_with_runner(
        mut run: impl FnMut(&str, &[&str]) -> io::Result<String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut muted_by_tray = None;
            for req in rx {
                match req {
                    MuteRequest::Set(true) => mute_sink(&mut muted_by_tray, &mut run),
                    MuteRequest::Set(false) => unmute_sink(&mut muted_by_tray, &mut run),
                    MuteRequest::Restore(ack) => {
                        unmute_sink(&mut muted_by_tray, &mut run);
                        // The caller may already have timed out waiting for this attempt.
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

    /// Restore the sink if muted by the tray, waiting at most five seconds.
    pub fn restore(&self) {
        self.restore_with_timeout(RESTORE_TIMEOUT);
    }

    fn restore_with_timeout(&self, timeout: Duration) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(MuteRequest::Restore(ack_tx)).is_err() {
            warn!("audio muter worker thread is gone; cannot restore audio sink");
            return;
        }
        match ack_rx.recv_timeout(timeout) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                warn!("timed out waiting for audio sink restoration");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn!("audio muter worker thread exited before confirming restore");
            }
        }
    }
}

fn mute_sink(
    muted_by_tray: &mut Option<MutedSink>,
    run: &mut impl FnMut(&str, &[&str]) -> io::Result<String>,
) {
    if muted_by_tray.is_some() {
        return;
    }
    if let Ok(stdout) = run("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"])
        && let Some(id) = stdout
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("id "))
            .and_then(|line| line.split_once(','))
            .and_then(|(id, _)| id.parse::<u32>().ok())
        && let Ok(stdout) = run("wpctl", &["get-volume", &id.to_string()])
    {
        if wpctl_muted(&stdout) {
            debug!(tool = "wpctl", "audio sink already muted");
            return;
        }
        if run("wpctl", &["set-mute", &id.to_string(), "1"]).is_ok() {
            *muted_by_tray = Some(MutedSink::Wpctl(id));
            debug!(tool = "wpctl", "muted audio sink");
            return;
        }
    }
    if let Ok(stdout) = run("pactl", &["get-default-sink"])
        && let name = stdout.trim()
        && !name.is_empty()
        && !name.starts_with('-')
        && !name.contains(char::is_whitespace)
        && let Ok(stdout) = run("pactl", &["get-sink-mute", name])
    {
        if pactl_muted(&stdout) {
            debug!(tool = "pactl", "audio sink already muted");
            return;
        }
        if run("pactl", &["set-sink-mute", name, "1"]).is_ok() {
            *muted_by_tray = Some(MutedSink::Pactl(name.to_owned()));
            debug!(tool = "pactl", "muted audio sink");
            return;
        }
    }
    warn!("failed to mute audio sink: neither wpctl nor pactl succeeded");
}

fn unmute_sink(
    muted_by_tray: &mut Option<MutedSink>,
    run: &mut impl FnMut(&str, &[&str]) -> io::Result<String>,
) {
    let Some(sink) = muted_by_tray.as_ref() else {
        return;
    };
    let (tool, result) = match sink {
        MutedSink::Wpctl(id) => ("wpctl", run("wpctl", &["set-mute", &id.to_string(), "0"])),
        MutedSink::Pactl(name) => ("pactl", run("pactl", &["set-sink-mute", name, "0"])),
    };
    match result {
        Ok(_) => {
            *muted_by_tray = None;
            debug!(tool, "restored audio sink");
        }
        Err(error) => warn!(tool, %error, "failed to restore audio sink"),
    }
}

fn run_audio_command(program: &str, args: &[&str]) -> io::Result<String> {
    let output = run_command(
        Command::new(program).args(args).stdout(Stdio::piped()),
        None,
        TOOL_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{program} exited with {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_times_out_when_child_never_reads() {
        let started = Instant::now();
        let error = run_command(
            Command::new("sh").args(["-c", "exec sleep 0.25"]),
            Some(&vec![b'x'; 1 << 20]),
            Duration::from_millis(40),
        )
        .expect_err("copy must time out without blocking on a full stdin pipe");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn probe_times_out_when_tool_never_exits() {
        let started = Instant::now();
        let error = run_command(
            Command::new("sh").args(["-c", "exec sleep 0.25"]),
            None,
            Duration::from_millis(40),
        )
        .expect_err("audio probe must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn clipboard_owner_does_not_inherit_a_captured_stdout_pipe() {
        let output = run_command(
            Command::new("sh")
                .args(["-c", "cat >/dev/null; sleep 0.25 &"])
                .stdout(Stdio::null()),
            Some(b"synthetic clipboard text"),
            Duration::from_millis(40),
        )
        .expect("a clipboard owner may outlive the command without holding an output pipe");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"");
    }

    #[test]
    fn restore_returns_when_worker_is_stuck() {
        let (tx, rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let MuteRequest::Restore(ack) = rx.recv().expect("restore request") else {
                panic!("expected restore request");
            };
            let _ = release_rx.recv_timeout(Duration::from_millis(250));
            let _ = ack.send(());
        });
        let muter = Muter { tx };
        let started = Instant::now();
        muter.restore_with_timeout(Duration::from_millis(40));
        let elapsed = started.elapsed();
        let _ = release_tx.send(());
        worker.join().expect("worker exits");
        assert!(
            elapsed < Duration::from_millis(200),
            "restore took {elapsed:?}"
        );
    }

    #[test]
    fn failed_restore_keeps_ownership_and_retries() {
        let sink = std::sync::Arc::new(std::sync::Mutex::new((false, 0)));
        let state = std::sync::Arc::clone(&sink);
        let muter = Muter::spawn_with_runner(move |program, args| {
            let mut state = state.lock().expect("sink lock");
            match (program, args) {
                ("wpctl", ["inspect", "@DEFAULT_AUDIO_SINK@"]) => {
                    Ok("id 42, type PipeWire:Interface:Node\n".into())
                }
                ("wpctl", ["get-volume", "42"]) => Ok("Volume: 1.00".into()),
                ("wpctl", ["set-mute", "42", "1"]) => {
                    state.0 = true;
                    Ok(String::new())
                }
                ("wpctl", ["set-mute", "42", "0"]) => {
                    state.1 += 1;
                    if state.1 == 1 {
                        Err(io::Error::other("temporary restore failure"))
                    } else {
                        state.0 = false;
                        Ok(String::new())
                    }
                }
                _ => panic!("unexpected audio command: {program} {args:?}"),
            }
        });
        muter.set(true);
        muter.set(false);
        muter.restore();
        assert_eq!(*sink.lock().expect("sink lock"), (false, 2));
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Sinks {
        default: usize,
        muted: [bool; 2],
        transitions: Vec<(usize, bool)>,
    }

    fn changing_default_muter(
        tool: &'static str,
        replacement_muted: bool,
    ) -> (Muter, std::sync::Arc<std::sync::Mutex<Sinks>>) {
        let sinks = std::sync::Arc::new(std::sync::Mutex::new(Sinks {
            default: 0,
            muted: [false, replacement_muted],
            transitions: Vec::new(),
        }));
        let state = std::sync::Arc::clone(&sinks);
        let muter = Muter::spawn_with_runner(move |program, args| {
            if program != tool {
                return Err(io::Error::other("tool unavailable"));
            }
            let mut state = state.lock().expect("sink state lock");
            let names = if tool == "wpctl" {
                ["42", "99"]
            } else {
                ["alsa_output.speakers", "bluez_output.headset"]
            };
            match args {
                ["inspect", "@DEFAULT_AUDIO_SINK@"] => Ok(format!(
                    "id {}, type PipeWire:Interface:Node\n",
                    names[state.default]
                )),
                ["get-default-sink"] => Ok(format!("{}\n", names[state.default])),
                [command, target] | [command, target, _] => {
                    let index = if target.starts_with('@') {
                        state.default
                    } else {
                        names
                            .iter()
                            .position(|name| name == target)
                            .expect("known sink")
                    };
                    match *command {
                        "get-volume" => Ok(if state.muted[index] {
                            "Volume: 0.75 [MUTED]\n".into()
                        } else {
                            "Volume: 0.75\n".into()
                        }),
                        "get-sink-mute" => Ok(if state.muted[index] {
                            "Mute: yes\n".into()
                        } else {
                            "Mute: no\n".into()
                        }),
                        "set-mute" | "set-sink-mute" => {
                            let muted = args[2] == "1";
                            state.muted[index] = muted;
                            state.transitions.push((index, muted));
                            if muted && index == 0 {
                                state.default = 1;
                            }
                            Ok(String::new())
                        }
                        _ => panic!("unexpected audio command: {program} {args:?}"),
                    }
                }
                _ => panic!("unexpected audio command: {program} {args:?}"),
            }
        });
        (muter, sinks)
    }

    #[test]
    fn default_change_restores_the_originally_muted_sink() {
        for tool in ["wpctl", "pactl"] {
            let (muter, sinks) = changing_default_muter(tool, false);
            muter.set(true);
            muter.set(false);
            muter.restore();
            assert_eq!(
                *sinks.lock().expect("sink state lock"),
                Sinks {
                    default: 1,
                    muted: [false, false],
                    transitions: vec![(0, true), (0, false)],
                },
                "{tool} must restore the original sink without touching the new default"
            );
        }
    }

    #[test]
    fn premuted_new_default_stays_muted() {
        for tool in ["wpctl", "pactl"] {
            let (muter, sinks) = changing_default_muter(tool, true);
            muter.set(true);
            muter.set(false);
            muter.set(true);
            muter.set(false);
            muter.restore();
            assert_eq!(
                *sinks.lock().expect("sink state lock"),
                Sinks {
                    default: 1,
                    muted: [false, true],
                    transitions: vec![(0, true), (0, false)],
                },
                "{tool} must leave the pre-muted replacement untouched across recordings"
            );
        }
    }

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
