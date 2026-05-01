//! Persist a slice of a terminal's PTY output to disk and replay it after
//! restart. The captured bytes are the same byte stream that originally fed
//! the terminal emulator; on replay they feed the vte parser directly via
//! `Terminal::process_output`, so the shell never sees them.
//!
//! Path layout: `{project_dir}/.vryn/scrollback/{snapshot_key}.snap`.

use std::path::{Path, PathBuf};

const SNAPSHOT_EXT: &str = "snap";
const SNAPSHOT_DIR: &str = ".vryn/scrollback";

/// File-format magic for vryn scrollback snapshots. Header layout:
///   [VRN4][u16 BE cols][u16 BE rows][u16 BE cwd_len][cwd_bytes...][PTY bytes]
///
/// `cols`/`rows` are retained as capture metadata for diagnostics and future
/// migrations. `cwd` is the working directory captured at quit time so the new
/// shell process starts where the last one was, mirroring VSCode's
/// `processDetails.cwd` revival.
///
/// `VRN4` intentionally invalidates earlier snapshots. `VRNS` encoded screen
/// columns as synthetic spaces/CUF sequences, `VRN2` could survive too long for
/// reused layout slots, and `VRN3` existed before project-wide close cleanup.
const SNAPSHOT_MAGIC: &[u8; 4] = b"VRN4";
/// Fixed-size prefix: 4 magic + 2 cols + 2 rows + 2 cwd_len = 10 bytes.
const SNAPSHOT_FIXED_LEN: usize = 10;

/// Build the snapshot file key from a stable terminal slot id.
pub fn snapshot_key(slot_id: &str) -> String {
    slot_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn snapshot_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{}.{}", key, SNAPSHOT_EXT))
}

pub fn project_snapshot_dir(project_path: impl AsRef<Path>) -> PathBuf {
    project_path.as_ref().join(SNAPSHOT_DIR)
}

/// Build a snapshot from raw PTY replay bytes. This mirrors VS Code's
/// persistent terminal path more closely than re-serializing the rendered grid:
/// capture the stream that fed the emulator, persist it, then feed it back into
/// a fresh emulator on restart.
pub fn capture_raw(
    replay_bytes: &[u8],
    cols: u16,
    rows: u16,
    max_lines: u32,
    cwd: Option<&str>,
) -> Vec<u8> {
    if max_lines == 0 || replay_bytes.is_empty() {
        return Vec::new();
    }

    let body = tail_lines(replay_bytes, max_lines);
    if body.is_empty() {
        return Vec::new();
    }

    let cwd_bytes: &[u8] = cwd.map(|s| s.as_bytes()).unwrap_or(&[]);
    let cwd_len_u16 = cwd_bytes.len().min(u16::MAX as usize) as u16;
    let cwd_bytes = &cwd_bytes[..cwd_len_u16 as usize];

    let mut out: Vec<u8> = Vec::with_capacity(SNAPSHOT_FIXED_LEN + cwd_bytes.len() + body.len());
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&cols.to_be_bytes());
    out.extend_from_slice(&rows.to_be_bytes());
    out.extend_from_slice(&cwd_len_u16.to_be_bytes());
    out.extend_from_slice(cwd_bytes);
    out.extend_from_slice(body);
    out
}

fn tail_lines(bytes: &[u8], max_lines: u32) -> &[u8] {
    if max_lines == 0 {
        return &[];
    }
    let mut scan_end = bytes.len();
    while scan_end > 0 && bytes[scan_end - 1] == b'\n' {
        scan_end -= 1;
    }
    let mut lines_seen = 0u32;
    for (idx, byte) in bytes[..scan_end].iter().enumerate().rev() {
        if *byte == b'\n' {
            lines_seen += 1;
            if lines_seen >= max_lines {
                return &bytes[idx + 1..];
            }
        }
    }
    bytes
}

fn append_line_boundary(out: &mut Vec<u8>) {
    if out.is_empty() || out.ends_with(b"\n") || out.ends_with(b"\r") {
        return;
    }
    out.extend_from_slice(b"\r\n");
}

/// Build a snapshot from already-persisted replay bytes plus newly captured
/// replay bytes. The line limit is applied to the combined stream, not to each
/// block separately.
pub fn capture_combined_raw(
    previous_replay_bytes: &[u8],
    new_replay_bytes: &[u8],
    cols: u16,
    rows: u16,
    max_lines: u32,
    cwd: Option<&str>,
) -> Vec<u8> {
    if previous_replay_bytes.is_empty() {
        return capture_raw(new_replay_bytes, cols, rows, max_lines, cwd);
    }
    if new_replay_bytes.is_empty() {
        return capture_raw(previous_replay_bytes, cols, rows, max_lines, cwd);
    }

    let mut combined = Vec::with_capacity(previous_replay_bytes.len() + 2 + new_replay_bytes.len());
    combined.extend_from_slice(previous_replay_bytes);
    append_line_boundary(&mut combined);
    combined.extend_from_slice(new_replay_bytes);
    capture_raw(&combined, cols, rows, max_lines, cwd)
}

/// Build a fresh snapshot by appending newly captured PTY output to the
/// previously persisted snapshot body, then keeping the last `max_lines`.
pub struct MergeCapture<'a> {
    pub dir: &'a Path,
    pub key: &'a str,
    pub restored_replay_bytes: &'a [u8],
    pub replay_bytes: &'a [u8],
    pub cols: u16,
    pub rows: u16,
    pub max_lines: u32,
    pub cwd: Option<&'a str>,
}

pub fn merge_existing_with_capture(input: MergeCapture<'_>) -> Vec<u8> {
    if input.max_lines == 0 {
        return Vec::new();
    }

    let previous = load(input.dir, input.key).map(|snapshot| snapshot.bytes).unwrap_or_default();
    let restored = capture_combined_raw(
        &previous,
        input.restored_replay_bytes,
        input.cols,
        input.rows,
        input.max_lines,
        None,
    );
    let restored_body = load_from_bytes(&restored).map(|snapshot| snapshot.bytes).unwrap_or_default();
    capture_combined_raw(
        &restored_body,
        input.replay_bytes,
        input.cols,
        input.rows,
        input.max_lines,
        input.cwd,
    )
}

fn load_from_bytes(raw: &[u8]) -> Option<LoadedSnapshot> {
    let (meta, body_start) = parse_header(raw)?;
    let bytes = raw[body_start..].to_vec();
    if bytes.is_empty() {
        return None;
    }
    Some(LoadedSnapshot {
        original_cols: meta.original_cols,
        original_rows: meta.original_rows,
        cwd: meta.cwd,
        bytes,
    })
}

/// Atomically write the captured bytes to `{dir}/{key}.snap`.
/// Empty input deletes any existing snapshot for this key.
pub fn save(dir: &Path, key: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = snapshot_path(dir, key);
    if bytes.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("snap.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Header-only metadata extracted from a snapshot file. Used by the
/// terminal-create path to read `cwd` *before* the PTY spawns, without
/// having to load the whole ANSI payload (which is replayed later).
pub struct SnapshotMeta {
    pub original_cols: u16,
    pub original_rows: u16,
    pub cwd: Option<String>,
}

/// A loaded snapshot: the metadata plus the ANSI byte stream that should
/// be replayed into a grid of those dimensions.
pub struct LoadedSnapshot {
    pub original_cols: u16,
    pub original_rows: u16,
    pub cwd: Option<String>,
    pub bytes: Vec<u8>,
}

fn parse_header(raw: &[u8]) -> Option<(SnapshotMeta, usize)> {
    if raw.len() < SNAPSHOT_FIXED_LEN || &raw[..4] != SNAPSHOT_MAGIC {
        return None;
    }
    let original_cols = u16::from_be_bytes([raw[4], raw[5]]);
    let original_rows = u16::from_be_bytes([raw[6], raw[7]]);
    let cwd_len = u16::from_be_bytes([raw[8], raw[9]]) as usize;
    let body_start = SNAPSHOT_FIXED_LEN + cwd_len;
    if raw.len() < body_start {
        return None;
    }
    let cwd = if cwd_len == 0 {
        None
    } else {
        std::str::from_utf8(&raw[SNAPSHOT_FIXED_LEN..body_start])
            .ok()
            .map(|s| s.to_string())
    };
    Some((
        SnapshotMeta {
            original_cols,
            original_rows,
            cwd,
        },
        body_start,
    ))
}

/// Read just the snapshot header (no ANSI payload) for callers that need
/// `cwd` before spawning the PTY. Returns `None` if the file is missing
/// or has the wrong magic.
pub fn peek_metadata(dir: &Path, key: &str) -> Option<SnapshotMeta> {
    let path = snapshot_path(dir, key);
    let mut buf = vec![0u8; SNAPSHOT_FIXED_LEN];
    use std::io::Read as _;
    let mut file = std::fs::File::open(&path).ok()?;
    file.read_exact(&mut buf).ok()?;
    let cwd_len = u16::from_be_bytes([buf[8], buf[9]]) as usize;
    if cwd_len > 0 {
        let mut cwd_buf = vec![0u8; cwd_len];
        file.read_exact(&mut cwd_buf).ok()?;
        buf.extend_from_slice(&cwd_buf);
    }
    parse_header(&buf).map(|(meta, _)| meta)
}

/// Read a snapshot if present and well-formed (correct magic). Snapshots
/// from older versions without a header are silently rejected.
pub fn load(dir: &Path, key: &str) -> Option<LoadedSnapshot> {
    let path = snapshot_path(dir, key);
    let raw = std::fs::read(&path).ok()?;
    let (meta, body_start) = parse_header(&raw)?;
    let bytes = raw[body_start..].to_vec();
    if bytes.is_empty() {
        return None;
    }
    Some(LoadedSnapshot {
        original_cols: meta.original_cols,
        original_rows: meta.original_rows,
        cwd: meta.cwd,
        bytes,
    })
}

/// Delete the snapshot for `key` (silent on absence).
pub fn clear(dir: &Path, key: &str) {
    let _ = std::fs::remove_file(snapshot_path(dir, key));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_raw_keeps_last_complete_lines() {
        let snapshot = capture_raw(b"one\r\ntwo\r\nthree\r\n", 80, 24, 2, None);
        assert_eq!(&snapshot[..4], b"VRN4");
        assert_eq!(&snapshot[SNAPSHOT_FIXED_LEN..], b"two\r\nthree\r\n");
    }

    #[test]
    fn load_rejects_legacy_grid_snapshots() {
        let dir = std::env::temp_dir().join(format!("vryn-snapshot-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = snapshot_path(&dir, "legacy");
        std::fs::write(&path, b"VRNS\0P\0\x18\0\0legacy body").unwrap();

        assert!(load(&dir, "legacy").is_none());

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn merge_existing_with_capture_keeps_last_n_lines_across_restarts() {
        let dir =
            std::env::temp_dir().join(format!("vryn-snapshot-merge-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = capture_raw(b"one\r\ntwo\r\nthree\r\n", 80, 24, 3, None);
        save(&dir, "term", &first).unwrap();

        let second = merge_existing_with_capture(MergeCapture {
            dir: &dir,
            key: "term",
            restored_replay_bytes: &[],
            replay_bytes: b"four\r\nfive\r\n",
            cols: 80,
            rows: 24,
            max_lines: 4,
            cwd: None,
        });
        save(&dir, "term", &second).unwrap();

        let third = merge_existing_with_capture(MergeCapture {
            dir: &dir,
            key: "term",
            restored_replay_bytes: &[],
            replay_bytes: b"six\r\n",
            cols: 80,
            rows: 24,
            max_lines: 4,
            cwd: None,
        });
        let (_, body_start) = parse_header(&third).unwrap();
        let body = &third[body_start..];

        assert!(
            !body.windows(b"one".len()).any(|w| w == b"one"),
            "oldest line should be trimmed"
        );
        for expected in [b"three" as &[u8], b"four", b"five", b"six"] {
            assert!(
                body.windows(expected.len()).any(|w| w == expected),
                "expected line missing: {:?}",
                String::from_utf8_lossy(expected)
            );
        }

        let _ = std::fs::remove_file(snapshot_path(&dir, "term"));
        let _ = std::fs::remove_dir(dir);
    }
}
