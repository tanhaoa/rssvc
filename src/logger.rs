//! Log file management.
//!
//! Each configured log path gets one dedicated writer thread. Reader threads
//! for the child's stdout/stderr pipes forward raw bytes through an mpsc
//! channel; the writer appends them to the file and rotates it when:
//!   - the file grows beyond `rotate_bytes` (if > 0), or
//!   - the local date changed since the file was opened (midnight rotation).
//!
//! Rotated files are archived next to the base file as
//! `<stem>-<YYYYMMDD_HHMMSS><ext>`; if `rotate_keep` > 0 the oldest archives
//! beyond that count are deleted.
//!
//! If stdout and stderr point to the same file, a single writer handles both
//! so the two streams never interleave-corrupt each other.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::util;

enum Msg {
    Data(Vec<u8>),
    Cfg { rotate_bytes: u32, rotate_keep: u32 },
}

/// A handle to one writer thread. Cheap to clone.
#[derive(Clone)]
pub struct Writer {
    tx: Sender<Msg>,
}

impl Writer {
    /// Spawn the writer thread. Returns `None` when thread creation fails so
    /// the caller can degrade gracefully (discard the stream) instead of
    /// panicking — with `panic = "abort"` a panic would take down the whole
    /// service process just because of a transient resource shortage.
    fn spawn(path: PathBuf, rotate_bytes: u32, rotate_keep: u32) -> Option<Writer> {
        let (tx, rx) = channel::<Msg>();
        let spawned = std::thread::Builder::new()
            .name(format!("rssvc-log({})", path.display()))
            .spawn(move || writer_loop(path, rotate_bytes, rotate_keep, rx));
        spawned.ok().map(|_| Writer { tx })
    }

    fn send(&self, b: Vec<u8>) {
        let _ = self.tx.send(Msg::Data(b));
    }

    fn update(&self, rotate_bytes: u32, rotate_keep: u32) {
        let _ = self.tx.send(Msg::Cfg {
            rotate_bytes,
            rotate_keep,
        });
    }
}

/// Manages writers for the stdout / stderr targets.
#[derive(Clone)]
pub struct Logger {
    writers: Vec<(String, Writer)>, // (lowercased path, writer)
    out: Option<usize>,
    err: Option<usize>,
}

impl Logger {
    /// Create writers. Empty paths mean "discard output".
    pub fn new(stdout: &str, stderr: &str, rotate_bytes: u32, rotate_keep: u32) -> Logger {
        let mut writers: Vec<(String, Writer)> = Vec::new();
        let mut out = None;
        let mut err = None;
        for (path, slot) in [(stdout, &mut out), (stderr, &mut err)] {
            if path.trim().is_empty() {
                continue;
            }
            let key = path.trim().to_lowercase();
            let idx = match writers.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    let Some(w) =
                        Writer::spawn(PathBuf::from(path.trim()), rotate_bytes, rotate_keep)
                    else {
                        // Thread creation failed: drop this stream instead
                        // of taking the service down.
                        continue;
                    };
                    writers.push((key.clone(), w));
                    writers.len() - 1
                }
            };
            *slot = Some(idx);
        }
        Logger { writers, out, err }
    }

    /// Clone of the stdout target writer (None if stdout is discarded).
    pub fn clone_out_writer(&self) -> Option<Writer> {
        self.out.map(|i| self.writers[i].1.clone())
    }

    /// Clone of the stderr target writer (None if stderr is discarded).
    pub fn clone_err_writer(&self) -> Option<Writer> {
        self.err.map(|i| self.writers[i].1.clone())
    }

    /// A service-level (rssvc lifecycle) message, written to every log file.
    pub fn service_line(&self, msg: &str) {
        if self.writers.is_empty() {
            return;
        }
        let st = util::now();
        let line = format!("[rssvc {}] {}\r\n", util::log_ts(&st), msg);
        for (_, w) in &self.writers {
            w.send(line.clone().into_bytes());
        }
    }

    /// Apply new rotation settings (used on SERVICE_CONTROL_PARAMCHANGE).
    pub fn update(&self, rotate_bytes: u32, rotate_keep: u32) {
        for (_, w) in &self.writers {
            w.update(rotate_bytes, rotate_keep);
        }
    }
}

fn open_append(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = fs::create_dir_all(dir);
        }
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn archive_name(path: &Path, stamp: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let mut candidate = path.with_file_name(format!("{stem}-{stamp}{ext}"));
    // Handle same-second collisions.
    let mut n = 1u32;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{stem}-{stamp}-{n}{ext}"));
        n += 1;
        if n > 1000 {
            break; // paranoia; never expected
        }
    }
    candidate
}

/// True for file names we ourselves generate as archives:
/// `{stem}-{YYYYMMDD_HHMMSS}{ext}` or `{stem}-{YYYYMMDD_HHMMSS}-{n}{ext}`.
/// Anything else is NOT ours, even when it shares the prefix and extension
/// (e.g. `app-debug.log` next to `app.log`) — pruning must never touch it.
fn is_archive_file(stem: &str, ext: &str, name: &str) -> bool {
    let prefix = format!("{stem}-");
    let Some(rest) = name.strip_prefix(&prefix) else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(ext) else {
        return false;
    };
    let b = rest.as_bytes();
    // Timestamp part: 8 digits + '_' + 6 digits.
    if b.len() < 15 {
        return false;
    }
    let digits = |s: &[u8]| s.iter().all(|c| c.is_ascii_digit());
    if !(digits(&b[0..8]) && b[8] == b'_' && digits(&b[9..15])) {
        return false;
    }
    if b.len() == 15 {
        return true;
    }
    // Optional same-second collision suffix: '-<digits>'.
    b[15] == b'-' && b.len() > 16 && digits(&b[16..])
}

fn prune_archives(base: &Path, keep: usize) {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = base
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let dir = match base.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => return,
    };
    let mut archives: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            // Strict match on our own archive naming pattern so unrelated
            // files (app-debug.log, app-old.log, ...) are never deleted.
            if is_archive_file(&stem, &ext, &name) {
                archives.push(p);
            }
        }
    }
    // Timestamped names sort lexically == chronologically.
    archives.sort();
    archives.reverse(); // newest first
    for p in archives.iter().skip(keep) {
        let _ = fs::remove_file(p);
    }
}

fn writer_loop(path: PathBuf, mut rotate_bytes: u32, mut rotate_keep: u32, rx: Receiver<Msg>) {
    let mut file = open_append(&path);
    let mut opened_at = util::now();
    let mut size = file
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .map(|m| m.len())
        .unwrap_or(0);

    // Exits when all senders are dropped (recv errors).
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Cfg {
                rotate_bytes: rb,
                rotate_keep: rk,
            } => {
                rotate_bytes = rb;
                rotate_keep = rk;
            }
            Msg::Data(b) => {
                // (Re)open if a previous open failed.
                if file.is_none() {
                    file = open_append(&path);
                    if let Some(f) = file.as_ref() {
                        opened_at = util::now();
                        size = f.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
                if file.is_none() {
                    continue; // nowhere to write; drop the bytes
                }
                let now = util::now();
                let rolled = util::date_code(&now) != util::date_code(&opened_at);
                let oversized = rotate_bytes > 0 && size + b.len() as u64 > rotate_bytes as u64;
                if rolled || oversized {
                    // Close before renaming (Windows refuses to rename open files).
                    drop(std::mem::take(&mut file));
                    let target = archive_name(&path, &util::datetime_code(&now));
                    // External readers (editors / viewers / antivirus) can hold
                    // the file open and make rename fail; retry briefly before
                    // giving up.
                    let mut renamed = false;
                    let mut last_err = None;
                    for attempt in 0..3 {
                        match fs::rename(&path, &target) {
                            Ok(()) => {
                                renamed = true;
                                break;
                            }
                            Err(e) => {
                                last_err = Some(e);
                                if attempt < 2 {
                                    std::thread::sleep(Duration::from_millis(250));
                                }
                            }
                        }
                    }
                    file = open_append(&path);
                    opened_at = now;
                    if renamed {
                        size = 0;
                    } else {
                        // Rotation failed. Keep the REAL size so the size
                        // trigger keeps firing (retry on every write) instead
                        // of silently disabling rotation and letting the file
                        // grow unbounded, and record a warning inside the log
                        // itself — the writer thread has no other channel.
                        size = file
                            .as_ref()
                            .and_then(|f| f.metadata().ok())
                            .map(|m| m.len())
                            .unwrap_or(0);
                        if let Some(f) = file.as_mut() {
                            let warn = format!(
                                "[rssvc] warning: 日志轮转失败 (目标: {}): {}\r\n",
                                target.display(),
                                last_err.map(|e| e.to_string()).unwrap_or_default()
                            );
                            let _ = f.write_all(warn.as_bytes());
                            let _ = f.flush();
                        }
                    }
                    if renamed && rotate_keep > 0 {
                        prune_archives(&path, rotate_keep as usize);
                    }
                }
                if let Some(f) = file.as_mut() {
                    if f.write_all(&b).is_ok() {
                        size += b.len() as u64;
                    }
                    let _ = f.flush();
                }
            }
        }
    }
}

/// Spawn a reader thread that drains the read end of a pipe and forwards
/// bytes to `writer`. Returns `None` when thread creation fails (caller
/// closes the read handle and logs; the service keeps running).
pub fn spawn_pipe_reader(
    read_handle: crate::util::SharedHandle,
    writer: Writer,
) -> Option<JoinHandle<()>> {
    use std::os::windows::io::FromRawHandle;
    // Convert the raw HANDLE into a std::fs::File OUTSIDE the closure:
    // File is Send, so it can move into the reader thread, whereas a raw
    // pointer captured directly would not be.
    let mut f = unsafe { File::from_raw_handle(read_handle.0 .0 as _) };
    std::thread::Builder::new()
        .name("rssvc-pipe-reader".to_string())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => writer.send(buf[..n].to_vec()),
                }
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_pattern_matches_only_our_names() {
        let stem = "app";
        let ext = ".log";
        // Our generated names.
        assert!(is_archive_file(stem, ext, "app-20260906_153045.log"));
        assert!(is_archive_file(stem, ext, "app-20260906_153045-1.log"));
        assert!(is_archive_file(stem, ext, "app-20260906_153045-42.log"));
        // Same prefix/extension but NOT ours: must never be pruned.
        assert!(!is_archive_file(stem, ext, "app-debug.log"));
        assert!(!is_archive_file(stem, ext, "app-old.log"));
        assert!(!is_archive_file(stem, ext, "app-2026.log"));
        assert!(!is_archive_file(stem, ext, "app-20260906.log"));
        assert!(!is_archive_file(stem, ext, "app-2026_99_9999.log"));
        assert!(!is_archive_file(stem, ext, "app-20260906_153045x.log"));
        assert!(!is_archive_file(stem, ext, "app-20260906_153045-.log"));
        assert!(!is_archive_file(stem, ext, "app.log"));
        assert!(!is_archive_file(stem, ext, "other-20260906_153045.log"));
        assert!(!is_archive_file(stem, ext, "app-20260906_153045.txt"));
        // Empty / tricky extensions.
        assert!(is_archive_file("svc", "", "svc-20260906_153045"));
        assert!(!is_archive_file("svc", "", "svc-20260906_15304"));
    }
}
