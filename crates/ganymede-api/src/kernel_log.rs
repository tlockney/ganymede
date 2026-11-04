//! A kernel's recent process output, for `GET /api/kernels/{id}/log`.
//!
//! Jupyter Server has no such API: a kernel's stderr reaches only the
//! server's own log. This keeps the last lines of each kernel's stdout and
//! stderr, and the host's lines about its process (start, exit, restart),
//! across restarts, so a client can show why a kernel died.

use ganymede_kernel_host::{KernelOutput, OutputSource};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Lines kept per kernel.
pub const LOG_LINES: usize = 1000;
/// Bytes of text kept per kernel, across its lines.
pub const LOG_BYTES: usize = 256 * 1024;
/// A longer line is cut to this many bytes.
pub const LINE_BYTES: usize = 8 * 1024;

/// One line of a kernel's log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelLogLine {
    /// Counts up from 1 over the kernel's life, restarts included. Pass the
    /// last one seen as `after` to get only newer lines.
    pub seq: u64,
    /// When the host read the line, as `now_iso` formats it.
    pub time: String,
    /// `stdout`, `stderr`, or `host` (the server, about the process).
    pub source: String,
    pub text: String,
}

/// `GET /api/kernels/{id}/log`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelLogModel {
    pub kernel_id: String,
    /// Oldest first.
    pub lines: Vec<KernelLogLine>,
    /// The `seq` of the newest line kept, 0 before the first; pass it as
    /// `after` on the next request.
    pub last_seq: u64,
    /// Why the log is empty when the kernel can't have one: `no_process`
    /// for a kernel the server does not run as a process of its own
    /// (in-process, or on a gateway).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
struct Lines {
    lines: VecDeque<KernelLogLine>,
    bytes: usize,
    last_seq: u64,
}

/// One kernel's log, bounded to [`LOG_LINES`] lines and [`LOG_BYTES`]
/// bytes of text; the oldest lines go first.
#[derive(Debug, Default)]
pub struct KernelLog(Mutex<Lines>);

impl KernelLog {
    pub fn push(&self, source: OutputSource, text: &str) {
        let mut text = text.to_string();
        if text.len() > LINE_BYTES {
            let mut end = LINE_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push('…');
        }
        let source = match source {
            OutputSource::Stdout => "stdout",
            OutputSource::Stderr => "stderr",
            OutputSource::Host => "host",
        };
        let mut l = self.0.lock().unwrap();
        l.last_seq += 1;
        l.bytes += text.len();
        let seq = l.last_seq;
        l.lines.push_back(KernelLogLine {
            seq,
            time: crate::now_iso(),
            source: source.into(),
            text,
        });
        while l.lines.len() > LOG_LINES || l.bytes > LOG_BYTES {
            let Some(old) = l.lines.pop_front() else {
                break;
            };
            l.bytes -= old.text.len();
        }
    }

    /// A line from the host about the kernel's process.
    pub fn host(&self, text: &str) {
        self.push(OutputSource::Host, text)
    }

    /// Where a provisioner sends this log's lines.
    pub fn output(self: &Arc<Self>) -> KernelOutput {
        let log = Arc::downgrade(self);
        KernelOutput::new(move |source, text| {
            if let Some(log) = log.upgrade() {
                log.push(source, text);
            }
        })
    }

    /// Up to `limit` lines, oldest first, and the newest `seq` kept. With
    /// `after` 0, the newest lines; otherwise the oldest with `seq` above
    /// `after`, so a client paging forward with the last `seq` it read
    /// misses none still kept.
    pub fn read(&self, after: u64, limit: usize) -> (Vec<KernelLogLine>, u64) {
        let l = self.0.lock().unwrap();
        let lines = if after == 0 {
            let skip = l.lines.len().saturating_sub(limit);
            l.lines.iter().skip(skip).cloned().collect()
        } else {
            // `seq`s are consecutive, so the first line after `after` is
            // found by arithmetic.
            let first = l.lines.front().map_or(0, |line| line.seq);
            let skip = usize::try_from(after.saturating_add(1).saturating_sub(first))
                .unwrap_or(usize::MAX);
            l.lines.iter().skip(skip).take(limit).cloned().collect()
        };
        (lines, l.last_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_lines_within_its_bounds() {
        let log = KernelLog::default();
        for i in 0..LOG_LINES + 5 {
            log.push(OutputSource::Stderr, &i.to_string());
        }
        let (lines, last) = log.read(0, usize::MAX);
        assert_eq!(lines.len(), LOG_LINES);
        assert_eq!(lines[0].text, "5");
        assert_eq!(last, LOG_LINES as u64 + 5);

        let log = KernelLog::default();
        let long = "é".repeat(LINE_BYTES);
        for _ in 0..LOG_BYTES / LINE_BYTES + 3 {
            log.push(OutputSource::Stdout, &long);
        }
        let (lines, _) = log.read(0, usize::MAX);
        assert!(lines.iter().map(|l| l.text.len()).sum::<usize>() <= LOG_BYTES);
        assert!(lines[0].text.ends_with('…'));
        assert!(lines[0].text.len() <= LINE_BYTES + '…'.len_utf8());
    }

    #[test]
    fn reads_after_a_seq_and_up_to_a_limit() {
        let log = KernelLog::default();
        for t in ["a", "b", "c", "d"] {
            log.host(t);
        }
        let texts = |(lines, _): (Vec<KernelLogLine>, u64)| {
            lines.into_iter().map(|l| l.text).collect::<Vec<_>>()
        };
        assert_eq!(texts(log.read(0, 2)), ["c", "d"]);
        assert_eq!(texts(log.read(2, 10)), ["c", "d"]);
        // Paging forward takes the oldest lines after `after`, so none
        // are skipped.
        assert_eq!(texts(log.read(1, 2)), ["b", "c"]);
        assert_eq!(texts(log.read(4, 10)), Vec::<String>::new());
        assert_eq!(log.read(4, 10).1, 4);
        // An `after` past any seq, even the largest, finds nothing.
        assert_eq!(texts(log.read(u64::MAX, 10)), Vec::<String>::new());
        // Lines dropped from the front: `after` before the oldest kept
        // starts at the oldest.
        let log = KernelLog::default();
        for i in 0..LOG_LINES + 10 {
            log.host(&i.to_string());
        }
        assert_eq!(texts(log.read(3, 1)), ["10"]);
    }
}
