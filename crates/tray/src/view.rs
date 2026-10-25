//! Presentation states used by the tray.

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
