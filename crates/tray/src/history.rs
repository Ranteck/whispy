//! History reader for recent dictations logged in `transcripts.jsonl`.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;

/// One accepted transcript from `transcripts.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// Transcribed text.
    pub text: String,
    /// Unix timestamp of the recording, in fractional seconds.
    pub timestamp: f64,
}

#[derive(Debug, serde::Deserialize)]
struct Record {
    #[serde(default)]
    timestamp: f64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    accepted: bool,
}

/// Read the most recent accepted transcripts from `path`, up to `limit` entries.
///
/// Returns transcripts in reverse chronological order (newest first).
/// If `path` does not exist or cannot be read, an empty vector is returned.
pub fn recent(path: &Path, limit: usize) -> Vec<Transcript> {
    if limit == 0 {
        return Vec::new();
    }

    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            if err.kind() != ErrorKind::NotFound {
                tracing::warn!(error_kind = ?err.kind(), "cannot open transcripts");
            }
            return Vec::new();
        }
    };

    let reader = BufReader::new(file);
    let mut ring = VecDeque::with_capacity(limit);

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                if err.kind() == ErrorKind::InvalidData {
                    continue;
                }
                tracing::warn!(error_kind = ?err.kind(), "stopped reading transcripts");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        if rec.accepted && !rec.text.trim().is_empty() {
            if ring.len() == limit {
                ring.pop_front();
            }
            ring.push_back(Transcript {
                text: rec.text,
                timestamp: rec.timestamp,
            });
        }
    }

    ring.into_iter().rev().collect()
}

/// Collapse whitespace and truncate text to at most `max` characters with an ellipsis.
///
/// If `text` exceeds `max` characters when whitespace is collapsed, the first
/// `max - 3` characters are kept and `"..."` is appended.
pub fn preview(text: &str, max: usize) -> String {
    let mut collapsed = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }

    let char_count = collapsed.chars().count();
    if char_count <= max {
        collapsed
    } else {
        let keep = max.saturating_sub(3);
        let mut out: String = collapsed.chars().take(keep).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("whispy-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recent_returns_last_accepted_newest_first() {
        let temp = TempDir::new("recent-newest-first");
        let log = temp.path().join("transcripts.jsonl");

        let mut content = String::new();
        for i in 1..=12 {
            let _ = writeln!(
                content,
                "{{\"text\": \"line {i}\", \"timestamp\": {i}.0, \"accepted\": true}}"
            );
        }
        std::fs::write(&log, content).expect("write transcripts.jsonl");

        let results = recent(&log, 10);
        assert_eq!(results.len(), 10);
        assert_eq!(results[0].text, "line 12");
        assert_eq!(results[0].timestamp, 12.0);
        assert_eq!(results[9].text, "line 3");
        assert_eq!(results[9].timestamp, 3.0);
    }

    #[test]
    fn skips_malformed_and_rejected() {
        let temp = TempDir::new("skips-malformed-rejected");
        let log = temp.path().join("transcripts.jsonl");

        let content = concat!(
            "not json at all\n",
            "{\"broken\": json\n",
            "{\"text\": \"rejected\", \"timestamp\": 1.0, \"accepted\": false}\n",
            "{\"text\": \"\", \"timestamp\": 2.0, \"accepted\": true}\n",
            "{\"text\": \"   \", \"timestamp\": 3.0, \"accepted\": true}\n",
            "\n",
            "{\"text\": \"valid dictation\", \"timestamp\": 4.0, \"accepted\": true}\n",
        );
        std::fs::write(&log, content).expect("write transcripts.jsonl");

        let results = recent(&log, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0],
            Transcript {
                text: "valid dictation".to_string(),
                timestamp: 4.0,
            }
        );
    }

    #[test]
    fn preview_truncates_by_chars() {
        assert_eq!(
            preview(&"ñ".repeat(50), 42),
            format!("{}...", "ñ".repeat(39))
        );
        assert_eq!(preview("a  b\nc", 42), "a b c");
    }

    #[test]
    fn recent_missing_file_returns_empty() {
        let results = recent(Path::new("/nonexistent/whispy/transcripts.jsonl"), 10);
        assert_eq!(results, []);
    }

    #[test]
    fn recent_on_directory_returns_empty() {
        let temp = TempDir::new("recent-dir");
        let results = recent(temp.path(), 10);
        assert_eq!(results, []);
    }

    #[test]
    fn skips_invalid_utf8_lines() {
        use std::io::Write as _;
        let temp = TempDir::new("invalid-utf8");
        let log = temp.path().join("transcripts.jsonl");

        let mut file = File::create(&log).expect("create file");
        file.write_all(b"{\"text\": \"first\", \"timestamp\": 1.0, \"accepted\": true}\n")
            .expect("write line 1");
        file.write_all(&[0xff, 0xff, b'\n'])
            .expect("write invalid utf8");
        file.write_all(b"{\"text\": \"second\", \"timestamp\": 2.0, \"accepted\": true}\n")
            .expect("write line 2");
        drop(file);

        let results = recent(&log, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].text, "second");
        assert_eq!(results[1].text, "first");
    }
}
