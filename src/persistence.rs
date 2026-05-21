//! File I/O for username inputs and result logs.
//!
//! Two on-disk formats are supported:
//!
//! - Plain text, append-only, one result per line, mirroring the
//!   legacy Python output so existing log readers don't break:
//!
//!   ```text
//!   # Discord username check results
//!   alice - TAKEN
//!   bob - AVAILABLE
//!   chr - INVALID: Username must be between 2 and 32 in length.
//!   ```
//!
//! - JSON Lines, one [`CheckResult`] per line, for downstream tooling.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use crate::result::CheckResult;

pub const TEXT_LOG_HEADER: &str = "# Discord username check results";

/// Append a batch of [`CheckResult`]s to a plain-text log.
///
/// The first time the file is created, a single header line is written
/// first. Subsequent appends just add lines, so the file accumulates
/// every check across runs.
pub fn append_text_log<P, I>(path: P, results: I) -> std::io::Result<usize>
where
    P: AsRef<Path>,
    I: IntoIterator<Item = CheckResult>,
{
    let path = path.as_ref();
    let exists = path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    if !exists {
        writeln!(file, "{TEXT_LOG_HEADER}")?;
    }

    let mut written = 0usize;
    for r in results {
        writeln!(file, "{}", r.format_file_line())?;
        written += 1;
    }
    Ok(written)
}

/// Append a batch as JSON Lines (one JSON object per line).
pub fn append_jsonl_log<P, I>(path: P, results: I) -> std::io::Result<usize>
where
    P: AsRef<Path>,
    I: IntoIterator<Item = CheckResult>,
{
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.as_ref())?;

    let mut written = 0usize;
    for r in results {
        let line = serde_json::to_string(&r)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{line}")?;
        written += 1;
    }
    Ok(written)
}

/// Parse a blob of user-supplied text into usernames.
///
/// Accepts newlines, commas, or whitespace as separators. Empty
/// fragments are dropped. Order is preserved. Lines starting with `#`
/// are treated as comments.
pub fn parse_usernames_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for piece in line.split(|c: char| c == ',' || c.is_whitespace()) {
            let piece = piece.trim();
            if !piece.is_empty() {
                out.push(piece.to_string());
            }
        }
    }
    out
}

/// Load and parse a usernames file (one per line, or comma-separated).
pub fn load_usernames_file<P: AsRef<Path>>(path: P) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut s = String::new();
    use std::io::Read;
    reader.read_to_string(&mut s)?;
    Ok(parse_usernames_text(&s))
}

/// Iterate a usernames file line-by-line without materializing the
/// entire list — useful for the exhaustive 4-letter (456,976 entry)
/// case where we want to start checking before the file is fully read.
pub fn stream_usernames_file<P, F>(path: P, mut on_each: F) -> std::io::Result<usize>
where
    P: AsRef<Path>,
    F: FnMut(String),
{
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut emitted = 0usize;
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        for piece in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
            let piece = piece.trim();
            if !piece.is_empty() {
                on_each(piece.to_string());
                emitted += 1;
            }
        }
    }
    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Status;
    use tempfile::tempdir;

    #[test]
    fn parses_mixed_separators() {
        let text = "alice, bob\ncharlie\n   dan , eve\n\n# comment line\nfrank";
        let names = parse_usernames_text(text);
        assert_eq!(
            names,
            vec!["alice", "bob", "charlie", "dan", "eve", "frank"]
        );
    }

    #[test]
    fn writes_header_only_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");

        let r1 = CheckResult::new("alice", Status::Available);
        let r2 = CheckResult::new("bob", Status::Taken);
        append_text_log(&path, vec![r1.clone()]).unwrap();
        append_text_log(&path, vec![r2.clone()]).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], TEXT_LOG_HEADER);
        assert_eq!(lines[1], "alice - AVAILABLE");
        assert_eq!(lines[2], "bob - TAKEN");
        // Header appears exactly once.
        assert_eq!(content.matches(TEXT_LOG_HEADER).count(), 1);
    }

    #[test]
    fn jsonl_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        let r = CheckResult::new(
            "alice",
            Status::Invalid {
                reason: "too short".into(),
            },
        );
        append_jsonl_log(&path, vec![r.clone()]).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: CheckResult = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed.username, "alice");
        assert!(matches!(parsed.status, Status::Invalid { .. }));
    }
}
