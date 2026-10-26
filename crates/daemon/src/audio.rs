//! `PipeWire` audio capture via `pw-record` (AU container or raw s16 PCM), with RMS metering,
//! digital gain, and clip-duration limits.
//!
//! `pw-record --format s16 -` streams AU-wrapped or raw PCM to stdout. A reader
//! thread sniffs the stream header, parses it into `i16` samples, applies `audio.gain`,
//! reports RMS every `audio.rms_interval_ms`, and caps the buffer at `audio.max_clip_secs`
//! (firing `on_too_long`). The first ~50 ms after the header are dropped to skip the USB codec's
//! stream-start transient, which would otherwise dominate peak normalization.
//!
//! On stop, the buffer is peak-normalized (only amplifying quiet clips) before
//! transcription — the mic records ~20-25 dB low (see docs/stt-benchmark.md).

use std::io::{BufReader, ErrorKind, Read};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use tracing::{debug, warn};

use crate::config::Audio;

/// Milliseconds dropped at capture start to skip the codec stream-start transient.
const SKIP_MS: u64 = 50;
/// Peak target for normalization (~ -0.1 dBFS).
const NORM_TARGET: f32 = 32_440.0;
/// Peaks at or below this are treated as silence and left unamplified.
const SILENCE_FLOOR: i32 = 200;

/// Flags shared between a [`Capture`] and its reader thread. `stop` is set by the
/// owner to request shutdown; `auto_stopped` and `speech_heard` are set by the
/// reader to report why it ended and whether it heard any speech.
#[derive(Clone, Default)]
struct Signals {
    stop: Arc<AtomicBool>,
    auto_stopped: Arc<AtomicBool>,
    speech_heard: Arc<AtomicBool>,
}

/// Spawns capture sessions using the configured audio parameters.
#[derive(Clone)]
pub struct Recorder {
    cfg: Audio,
}

impl Recorder {
    pub fn new(cfg: Audio) -> Self {
        Self { cfg }
    }

    /// Start capturing. `on_rms` is called roughly every `rms_interval_ms` with a
    /// normalized RMS in `[0, 1]`. `on_too_long` fires once if the clip reaches
    /// `max_clip_secs` (capture stops accumulating). `on_silence` fires once if,
    /// after speech has been heard, the clip goes silent for `silence_timeout_ms`.
    pub fn start<R, M, S>(
        &self,
        on_rms: R,
        on_too_long: M,
        on_silence: S,
    ) -> std::io::Result<Capture>
    where
        R: Fn(f32) + Send + 'static,
        M: FnOnce() + Send + 'static,
        S: FnOnce() + Send + 'static,
    {
        let mut child = Command::new("pw-record")
            .arg("--rate")
            .arg(self.cfg.rate.to_string())
            .arg("--channels")
            .arg(self.cfg.channels.to_string())
            .arg("--format")
            .arg("s16")
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout piped");
        let signals = Signals::default();

        let cfg = self.cfg.clone();
        let reader_signals = signals.clone();
        let reader = std::thread::spawn(move || {
            read_loop(
                stdout,
                &cfg,
                reader_signals,
                on_rms,
                on_too_long,
                on_silence,
            )
        });

        debug!(pid = child.id(), "capture started");
        Ok(Capture {
            child,
            reader: Some(reader),
            signals,
        })
    }
}

/// A running capture: the `pw-record` child plus its reader thread.
pub struct Capture {
    child: Child,
    reader: Option<JoinHandle<Vec<i16>>>,
    signals: Signals,
}

impl Capture {
    /// True if capture hit the max-duration cap.
    pub fn auto_stopped(&self) -> bool {
        self.signals.auto_stopped.load(Ordering::Relaxed)
    }

    /// Stop capture and return the peak-normalized samples, or `None` if the clip
    /// is too short (`< min_samples`) or contained no detected speech — both
    /// dropped silently so a silent clip is never sent to whisper (which would
    /// confidently hallucinate "Thank you." / "All right." on pure silence).
    pub fn finish(mut self, min_samples: usize) -> Option<Vec<i16>> {
        let buf = self.teardown();
        let speech = self.signals.speech_heard.load(Ordering::Relaxed);
        debug!(
            samples = buf.len(),
            min = min_samples,
            speech,
            "capture finished"
        );
        if buf.len() < min_samples || !speech {
            return None;
        }
        Some(normalize_peak(buf))
    }

    /// Stop capture and discard the buffer.
    pub fn cancel(mut self) {
        let _ = self.teardown();
        debug!("capture cancelled");
    }

    /// Signal stop, kill `pw-record` (closing the pipe -> reader EOF), and join.
    fn teardown(&mut self) -> Vec<i16> {
        self.signals.stop.store(true, Ordering::Relaxed);
        self.child.kill().ok();
        let buf = self
            .reader
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        self.child.wait().ok();
        buf
    }
}

/// Standard AU container header length in bytes (magic + 5 u32 fields).
const AU_HEADER_LEN: usize = 24;

/// Byte order of incoming audio samples.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Endian {
    Big,
    Little,
}

impl Endian {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Big => "big",
            Self::Little => "little",
        }
    }
}

/// Sniffs the start of the audio stream for a valid AU container or raw PCM.
fn sniff_header(buf: &[u8], eof: bool, cfg: &Audio) -> Option<(usize, Endian)> {
    // PipeWire 1.6.10+ writes AU in the container's big-endian byte order (.snd);
    // older versions wrote AU little-endian (dns.) or raw PCM.
    let endian = if buf.starts_with(b".snd") {
        Some(Endian::Big)
    } else if buf.starts_with(b"dns.") {
        Some(Endian::Little)
    } else {
        None
    };

    let Some(endian) = endian else {
        // If the buffer cannot yet be ruled out as an AU magic prefix and more data may arrive, wait.
        if !eof && buf.len() < 4 && (b".snd".starts_with(buf) || b"dns.".starts_with(buf)) {
            return None;
        }
        return Some((0, Endian::Little));
    };

    if buf.len() < AU_HEADER_LEN {
        if !eof {
            return None;
        }
        // Legacy raw PCM can start with bytes that look like AU magic.
        warn!(
            fields = "header_len",
            len = buf.len(),
            expected = AU_HEADER_LEN,
            "AU header candidate truncated; falling back to raw PCM"
        );
        return Some((0, Endian::Little));
    }

    let (offset, encoding, rate, channels) = match endian {
        Endian::Big => (
            u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
            u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]),
            u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]),
        ),
        Endian::Little => (
            u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
        ),
    };

    let offset_ok = offset == 24;
    let encoding_ok = encoding == 3;
    let rate_ok = rate == cfg.rate;
    let channels_ok = channels == u32::from(cfg.channels);

    if offset_ok && encoding_ok && rate_ok && channels_ok {
        Some((AU_HEADER_LEN, endian))
    } else {
        let mut mismatched = Vec::new();
        if !offset_ok {
            mismatched.push("offset");
        }
        if !encoding_ok {
            mismatched.push("encoding");
        }
        if !rate_ok {
            mismatched.push("rate");
        }
        if !channels_ok {
            mismatched.push("channels");
        }
        let fields = mismatched.join(", ");

        // Legacy raw PCM can start with bytes that look like AU magic.
        warn!(
            fields = %fields,
            offset,
            encoding,
            rate,
            channels,
            "invalid AU header; falling back to raw PCM"
        );
        Some((0, Endian::Little))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "stateful audio streaming loop coordinating header sniffing, transient skip, RMS metering, and silence auto-stop"
)]
fn read_loop<R, M, S>(
    stdout: impl Read,
    cfg: &Audio,
    signals: Signals,
    on_rms: R,
    on_too_long: M,
    on_silence: S,
) -> Vec<i16>
where
    R: Fn(f32),
    M: FnOnce(),
    S: FnOnce(),
{
    let mut reader = BufReader::new(stdout);
    let mut chunk = [0u8; 8192];
    let mut leftover: Vec<u8> = Vec::new();

    let mut samples: Vec<i16> = Vec::with_capacity(cfg.rate as usize * 4);
    let window_len = cfg.rms_window();
    let max_samples = cfg.max_samples();
    let skip_samples = (u64::from(cfg.rate) * SKIP_MS / 1000) as usize;
    let gain = cfg.gain;

    let mut header_discard = 0usize;
    let mut endian = Endian::Little;
    let mut format_detected = false;

    // Speech-energy bookkeeping. A window above `silence_threshold` counts as
    // speech; `speech_seen` gates both the no-speech drop (a clip that never
    // crossed the threshold is silence) and arming the trailing-silence auto-stop
    // (so lead-in silence before the user starts talking is ignored).
    let silence_limit = cfg.silence_windows();
    let silence_threshold = cfg.silence_rms_threshold;
    let mut speech_seen = false;
    let mut silent_windows = 0usize;
    let mut hit_silence = false;

    let mut skipped = 0usize;
    let mut sq_sum = 0.0f64;
    let mut window_count = 0usize;
    let mut too_long = false;

    'outer: loop {
        if signals.stop.load(Ordering::Relaxed) {
            break;
        }
        let eof = match reader.read(&mut chunk) {
            Ok(0) => true,
            Ok(n) => {
                leftover.extend_from_slice(&chunk[..n]);
                false
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                warn!(error = %e, "capture read error");
                true
            }
        };

        if !format_detected {
            if let Some((discard, end)) = sniff_header(&leftover, eof, cfg) {
                let container = if discard > 0 { "au" } else { "raw" };
                debug!(
                    container,
                    byte_order = end.as_str(),
                    "stream container detected"
                );
                header_discard = discard;
                endian = end;
                format_detected = true;
            } else {
                continue;
            }
        }

        if header_discard > 0 {
            let to_drop = leftover.len().min(header_discard);
            leftover.drain(..to_drop);
            header_discard -= to_drop;
        }

        if header_discard == 0 {
            let full = leftover.len() - (leftover.len() % 2);
            for pair in leftover[..full].as_chunks::<2>().0 {
                // Drop the stream-start transient.
                if skipped < skip_samples {
                    skipped += 1;
                    continue;
                }
                let raw = match endian {
                    Endian::Big => i16::from_be_bytes([pair[0], pair[1]]),
                    Endian::Little => i16::from_le_bytes([pair[0], pair[1]]),
                };
                let sample = apply_gain(raw, gain);
                samples.push(sample);

                let norm = f64::from(sample) / 32768.0;
                sq_sum += norm * norm;
                window_count += 1;
                if window_count >= window_len {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "RMS of normalized samples lies in [0, 1]; f32 is plenty"
                    )]
                    let rms = ((sq_sum / window_count as f64).sqrt() as f32).clamp(0.0, 1.0);
                    on_rms(rms);
                    sq_sum = 0.0;
                    window_count = 0;

                    if rms > silence_threshold {
                        speech_seen = true;
                        silent_windows = 0;
                    } else if speech_seen {
                        // Trailing silence after speech: arm the auto-stop if enabled.
                        silent_windows += 1;
                        if let Some(limit) = silence_limit
                            && silent_windows >= limit
                        {
                            hit_silence = true;
                            break 'outer;
                        }
                    }
                }

                if samples.len() >= max_samples {
                    too_long = true;
                    break 'outer;
                }
            }
            leftover.drain(..full);
        }

        if eof {
            break;
        }
    }

    signals.speech_heard.store(speech_seen, Ordering::Relaxed);

    if too_long {
        signals.auto_stopped.store(true, Ordering::Relaxed);
        on_too_long();
    } else if hit_silence {
        debug!(silent_windows, "trailing-silence auto-stop");
        on_silence();
    }
    samples
}

/// Apply a linear gain with saturation.
#[expect(
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    reason = "1.0 is an exact config value meaning 'no gain'; the result is clamped to i16 range before the cast"
)]
fn apply_gain(sample: i16, gain: f32) -> i16 {
    if gain == 1.0 {
        return sample;
    }
    (f32::from(sample) * gain).round().clamp(-32768.0, 32767.0) as i16
}

/// Peak-normalize toward [`NORM_TARGET`]. Only amplifies (quiet clips); never
/// attenuates, and leaves near-silent buffers untouched.
fn normalize_peak(mut buf: Vec<i16>) -> Vec<i16> {
    let peak = buf
        .iter()
        .map(|s| i32::from(s.unsigned_abs()))
        .max()
        .unwrap_or(0);
    if peak <= SILENCE_FLOOR {
        return buf;
    }
    let factor = NORM_TARGET / peak as f32;
    if factor <= 1.0 {
        return buf;
    }
    debug!(peak, factor, "peak-normalizing clip");
    for s in &mut buf {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to i16 range before the cast"
        )]
        let scaled = (f32::from(*s) * factor).round().clamp(-32768.0, 32767.0) as i16;
        *s = scaled;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_saturates() {
        assert_eq!(apply_gain(20000, 2.0), 32767);
        assert_eq!(apply_gain(-20000, 2.0), -32768);
        assert_eq!(apply_gain(100, 1.0), 100);
    }

    #[test]
    fn normalize_amplifies_quiet_only() {
        // Quiet clip gets amplified so its peak approaches the target.
        let out = normalize_peak(vec![0, 1000, -1000, 500]);
        let peak = out
            .iter()
            .map(|s| i32::from(s.unsigned_abs()))
            .max()
            .unwrap_or(0);
        assert!(peak > 30_000, "quiet clip should be amplified, peak={peak}");

        // A clip already at/above the target peak is left as-is (never attenuated).
        let loud = vec![0, 32600, -32600];
        assert_eq!(normalize_peak(loud.clone()), loud);

        // Near-silence is untouched.
        let silence = vec![0, 50, -40];
        assert_eq!(normalize_peak(silence.clone()), silence);
    }

    fn test_audio_config() -> Audio {
        let cfg: crate::config::Config =
            toml::from_str(crate::config::default_config_toml()).expect("default.toml must parse");
        cfg.audio
    }

    fn make_au_header(be: bool, offset: u32) -> Vec<u8> {
        let mut header = Vec::with_capacity(24);
        if be {
            header.extend_from_slice(b".snd");
            header.extend_from_slice(&offset.to_be_bytes());
            header.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
            header.extend_from_slice(&3u32.to_be_bytes());
            header.extend_from_slice(&16000u32.to_be_bytes());
            header.extend_from_slice(&1u32.to_be_bytes());
        } else {
            header.extend_from_slice(b"dns.");
            header.extend_from_slice(&offset.to_le_bytes());
            header.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
            header.extend_from_slice(&3u32.to_le_bytes());
            header.extend_from_slice(&16000u32.to_le_bytes());
            header.extend_from_slice(&1u32.to_le_bytes());
        }
        header
    }

    struct ChunkedReader<R> {
        inner: R,
        max_chunk: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let cap = buf.len().min(self.max_chunk);
            self.inner.read(&mut buf[..cap])
        }
    }

    #[test]
    fn au_big_endian_stream_decodes_samples() {
        let cfg = test_audio_config();
        let mut stream = make_au_header(true, 24);
        for _ in 0..2400 {
            stream.extend_from_slice(&1000i16.to_be_bytes());
        }
        let samples = read_loop(&stream[..], &cfg, Signals::default(), |_| {}, || {}, || {});
        for &s in &samples {
            assert_eq!(s, 1000);
        }
        assert_eq!(samples.len(), 1600);
    }

    #[test]
    fn au_little_endian_stream_skips_header() {
        let cfg = test_audio_config();
        let mut stream = make_au_header(false, 24);
        for _ in 0..2400 {
            stream.extend_from_slice(&1000i16.to_le_bytes());
        }
        let samples = read_loop(&stream[..], &cfg, Signals::default(), |_| {}, || {}, || {});
        for &s in &samples {
            assert_eq!(s, 1000);
        }
        assert_eq!(samples.len(), 1600);
    }

    #[test]
    fn raw_stream_decodes_little_endian() {
        let cfg = test_audio_config();
        let mut stream = Vec::new();
        for _ in 0..2400 {
            stream.extend_from_slice(&1000i16.to_le_bytes());
        }
        let samples = read_loop(&stream[..], &cfg, Signals::default(), |_| {}, || {}, || {});
        for &s in &samples {
            assert_eq!(s, 1000);
        }
        assert_eq!(samples.len(), 1600);
    }

    #[test]
    fn au_header_split_across_reads() {
        let cfg = test_audio_config();
        let mut stream = make_au_header(true, 24);
        for _ in 0..2400 {
            stream.extend_from_slice(&1000i16.to_be_bytes());
        }
        let reader = ChunkedReader {
            inner: &stream[..],
            max_chunk: 3,
        };
        let samples = read_loop(reader, &cfg, Signals::default(), |_| {}, || {}, || {});
        for &s in &samples {
            assert_eq!(s, 1000);
        }
        assert_eq!(samples.len(), 1600);
    }

    fn make_custom_au_header(
        be: bool,
        offset: u32,
        size: u32,
        encoding: u32,
        rate: u32,
        channels: u32,
    ) -> Vec<u8> {
        let mut header = Vec::with_capacity(24);
        let magic = if be { b".snd" } else { b"dns." };
        header.extend_from_slice(magic);
        if be {
            header.extend_from_slice(&offset.to_be_bytes());
            header.extend_from_slice(&size.to_be_bytes());
            header.extend_from_slice(&encoding.to_be_bytes());
            header.extend_from_slice(&rate.to_be_bytes());
            header.extend_from_slice(&channels.to_be_bytes());
        } else {
            header.extend_from_slice(&offset.to_le_bytes());
            header.extend_from_slice(&size.to_le_bytes());
            header.extend_from_slice(&encoding.to_le_bytes());
            header.extend_from_slice(&rate.to_le_bytes());
            header.extend_from_slice(&channels.to_le_bytes());
        }
        header
    }

    #[test]
    fn invalid_au_headers_fall_back_to_raw_little_endian() {
        let cfg = test_audio_config();
        let skip_samples = (u64::from(cfg.rate) * SKIP_MS / 1000) as usize;

        struct TestCase {
            name: &'static str,
            prefix: Vec<u8>,
            has_payload: bool,
        }

        let mut cases = Vec::new();

        for be in [true, false] {
            let magic_name = if be { "be" } else { "le" };

            // Invalid offsets.
            for &offset in &[0, 1, 23, 25, u32::MAX] {
                cases.push(TestCase {
                    name: Box::leak(format!("{magic_name} offset {offset}").into_boxed_str()),
                    prefix: make_custom_au_header(
                        be,
                        offset,
                        0xffff_ffff,
                        3,
                        cfg.rate,
                        u32::from(cfg.channels),
                    ),
                    has_payload: true,
                });
            }

            // Wrong encoding (with offset 24).
            cases.push(TestCase {
                name: Box::leak(format!("{magic_name} wrong encoding").into_boxed_str()),
                prefix: make_custom_au_header(
                    be,
                    24,
                    0xffff_ffff,
                    1,
                    cfg.rate,
                    u32::from(cfg.channels),
                ),
                has_payload: true,
            });

            // Wrong rate (with offset 24).
            cases.push(TestCase {
                name: Box::leak(format!("{magic_name} wrong rate").into_boxed_str()),
                prefix: make_custom_au_header(
                    be,
                    24,
                    0xffff_ffff,
                    3,
                    cfg.rate + 1,
                    u32::from(cfg.channels),
                ),
                has_payload: true,
            });

            // Wrong channels (with offset 24).
            cases.push(TestCase {
                name: Box::leak(format!("{magic_name} wrong channels").into_boxed_str()),
                prefix: make_custom_au_header(
                    be,
                    24,
                    0xffff_ffff,
                    3,
                    cfg.rate,
                    u32::from(cfg.channels) + 1,
                ),
                has_payload: true,
            });
        }

        // Truncated (<24 byte) magic-prefixed candidates.
        cases.push(TestCase {
            name: "be truncated 4 bytes",
            prefix: b".snd".to_vec(),
            has_payload: false,
        });
        cases.push(TestCase {
            name: "be truncated 8 bytes",
            prefix: b".snd\0\0\0\x18".to_vec(),
            has_payload: false,
        });
        cases.push(TestCase {
            name: "le truncated 4 bytes",
            prefix: b"dns.".to_vec(),
            has_payload: false,
        });
        cases.push(TestCase {
            name: "le truncated 8 bytes",
            prefix: b"dns.\x18\0\0\0".to_vec(),
            has_payload: false,
        });

        for tc in cases {
            let mut stream = tc.prefix;
            if tc.has_payload {
                for i in 0..2400i16 {
                    stream.extend_from_slice(&i.to_le_bytes());
                }
            }

            let expected: Vec<i16> = stream
                .as_chunks::<2>()
                .0
                .iter()
                .skip(skip_samples)
                .map(|p| i16::from_le_bytes(*p))
                .collect();

            let samples = read_loop(&stream[..], &cfg, Signals::default(), |_| {}, || {}, || {});
            assert_eq!(samples, expected, "failed for test case: {}", tc.name);
        }
    }

    #[test]
    fn au_big_endian_hits_max_duration() {
        use std::cell::Cell;

        let mut cfg = test_audio_config();
        cfg.max_clip_secs = 0.1;
        let max_samples = cfg.max_samples();
        assert_eq!(max_samples, 1600);

        let mut stream = make_au_header(true, 24);
        for i in 0..3000i16 {
            stream.extend_from_slice(&i.to_be_bytes());
        }

        let signals = Signals::default();
        let too_long_count = Cell::new(0usize);
        let silence_count = Cell::new(0usize);

        let samples = read_loop(
            &stream[..],
            &cfg,
            signals.clone(),
            |_| {},
            || too_long_count.set(too_long_count.get() + 1),
            || silence_count.set(silence_count.get() + 1),
        );

        assert_eq!(
            too_long_count.get(),
            1,
            "on_too_long must be called exactly once"
        );
        assert_eq!(silence_count.get(), 0, "on_silence must not be called");
        assert!(
            signals.auto_stopped.load(Ordering::Relaxed),
            "auto_stopped signal must be set"
        );
        assert_eq!(
            samples.len(),
            max_samples,
            "returned samples must equal cfg.max_samples()"
        );
    }

    #[test]
    fn au_big_endian_speech_then_silence_auto_stops() {
        use std::cell::Cell;

        let mut cfg = test_audio_config();
        cfg.silence_timeout_ms = 160;

        let mut stream = make_au_header(true, 24);
        for _ in 0..800 {
            stream.extend_from_slice(&10000i16.to_be_bytes());
        }
        for _ in 0..1280 {
            stream.extend_from_slice(&10000i16.to_be_bytes());
        }
        for _ in 0..3000 {
            stream.extend_from_slice(&0i16.to_be_bytes());
        }

        let signals = Signals::default();
        let too_long_count = Cell::new(0usize);
        let silence_count = Cell::new(0usize);

        let _samples = read_loop(
            &stream[..],
            &cfg,
            signals.clone(),
            |_| {},
            || too_long_count.set(too_long_count.get() + 1),
            || silence_count.set(silence_count.get() + 1),
        );

        assert_eq!(
            silence_count.get(),
            1,
            "on_silence must be called exactly once"
        );
        assert_eq!(too_long_count.get(), 0, "on_too_long must not be called");
        assert!(
            signals.speech_heard.load(Ordering::Relaxed),
            "speech_heard must be true"
        );
        assert!(
            !signals.auto_stopped.load(Ordering::Relaxed),
            "auto_stopped must not be set on silence stop"
        );
    }
}
