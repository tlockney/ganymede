//! Run a notebook's code cells on a [`KernelSession`] and store what comes
//! back as nbformat outputs. Language-neutral: it only translates iopub
//! messages into output records.
//!
//! Rules, from `nbclient` and the notebook UI:
//! - `clear_output` with `wait` clears when the next output arrives.
//! - `update_display_data` replaces every output that was created with the
//!   same `display_id`.
//! - Consecutive `stream` chunks on the same stream are merged when
//!   [`Options::coalesce_streams`] is set (the default here; `nbclient`
//!   defaults to off).
//! - An error reply stops execution unless [`Options::allow_errors`].

use jcore_kernel_host::protocol::content::{self, ExecuteRequest, ReplyStatus};
use jcore_kernel_host::protocol::Message;
use jcore_kernel_host::KernelSession;
use jcore_nbformat::{
    Cell, CodeCell, DisplayDataOutput, ErrorOutput, ExecuteResultOutput, MimeBundle,
    MultilineString, Notebook, Output, StreamOutput,
};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kernel: {0}")]
    Kernel(#[from] jcore_kernel_host::Error),
    #[error("cell {index} failed: {ename}: {evalue}")]
    CellFailed {
        index: usize,
        ename: String,
        evalue: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Options {
    pub timeout: Duration,
    pub allow_errors: bool,
    pub coalesce_streams: bool,
    /// Record `execution` timing metadata on each cell, as JupyterLab does.
    pub record_timing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: Duration::from_secs(600),
            allow_errors: false,
            coalesce_streams: true,
            record_timing: false,
        }
    }
}

/// What happened, cell by cell.
#[derive(Debug, Default)]
pub struct Report {
    pub executed: usize,
    pub errored: Vec<usize>,
    /// Index of the cell that stopped execution, if any.
    pub stopped_at: Option<usize>,
}

/// Execute every code cell in order, writing outputs into `nb`.
pub async fn execute_notebook(
    session: &KernelSession,
    nb: &mut Notebook,
    opts: &Options,
) -> Result<Report> {
    let mut report = Report::default();
    for (index, cell) in nb.cells.iter_mut().enumerate() {
        let Cell::Code(code) = cell else { continue };
        if code.source.as_str().trim().is_empty() {
            code.outputs.clear();
            code.execution_count = None;
            continue;
        }
        let failed = execute_cell(session, code, opts).await?;
        report.executed += 1;
        if failed {
            report.errored.push(index);
            if !opts.allow_errors {
                report.stopped_at = Some(index);
                let (ename, evalue) = code
                    .outputs
                    .iter()
                    .rev()
                    .find_map(|o| match o {
                        Output::Error(e) => Some((e.ename.clone(), e.evalue.clone())),
                        _ => None,
                    })
                    .unwrap_or_default();
                return Err(Error::CellFailed {
                    index,
                    ename,
                    evalue,
                });
            }
        }
    }
    Ok(report)
}

/// Execute one cell. Returns true when the reply status was not `ok`.
pub async fn execute_cell(
    session: &KernelSession,
    cell: &mut CodeCell,
    opts: &Options,
) -> Result<bool> {
    cell.outputs.clear();
    cell.execution_count = None;
    let mut req = ExecuteRequest::new(cell.source.as_str().to_string());
    req.allow_stdin = false;
    req.stop_on_error = !opts.allow_errors;
    let started = chrono_now();
    let exec = session.execute(req, opts.timeout).await?;
    let mut builder = OutputBuilder::new(opts.coalesce_streams);
    for m in &exec.outputs {
        builder.push(m);
    }
    cell.outputs = builder.finish();
    let reply: content::ExecuteReply = exec
        .reply
        .content_as()
        .map_err(jcore_kernel_host::Error::from)?;
    cell.execution_count = reply.execution_count;
    if opts.record_timing {
        let timing = cell
            .metadata
            .entry("execution")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(t) = timing.as_object_mut() {
            t.insert(
                "iopub.execute_input".into(),
                serde_json::Value::String(started),
            );
            t.insert(
                "shell.execute_reply".into(),
                serde_json::Value::String(chrono_now()),
            );
        }
    }
    Ok(reply.status != ReplyStatus::Ok)
}

fn chrono_now() -> String {
    // ISO 8601 without pulling chrono into this crate: reuse the header's
    // date format via a throwaway header.
    jcore_kernel_host::protocol::Header::new("", "", "").date
}

/// Turns a cell's iopub messages into nbformat outputs.
pub struct OutputBuilder {
    outputs: Vec<Output>,
    display_ids: HashMap<String, Vec<usize>>,
    clear_pending: bool,
    coalesce: bool,
}

impl OutputBuilder {
    pub fn new(coalesce_streams: bool) -> Self {
        OutputBuilder {
            outputs: Vec::new(),
            display_ids: HashMap::new(),
            clear_pending: false,
            coalesce: coalesce_streams,
        }
    }

    pub fn push(&mut self, m: &Message) {
        match m.msg_type() {
            "clear_output" => {
                let wait = m
                    .content
                    .get("wait")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if wait {
                    self.clear_pending = true;
                } else {
                    self.clear();
                }
            }
            "stream" => {
                let Ok(s) = m.content_as::<content::Stream>() else {
                    return;
                };
                self.before_output();
                if self.coalesce {
                    if let Some(Output::Stream(last)) = self.outputs.last_mut() {
                        if last.name == s.name {
                            last.text.as_mut_string().push_str(&s.text);
                            return;
                        }
                    }
                }
                self.outputs.push(Output::Stream(StreamOutput {
                    name: s.name,
                    text: MultilineString::new(s.text),
                    extra: Default::default(),
                }));
            }
            "display_data" => {
                let Ok(d) = m.content_as::<content::DisplayData>() else {
                    return;
                };
                self.before_output();
                let out = DisplayDataOutput {
                    data: bundle(d.data),
                    metadata: d.metadata,
                    extra: Default::default(),
                };
                let idx = self.outputs.len();
                self.outputs.push(Output::DisplayData(out));
                if let Some(id) = d.transient.and_then(|t| t.display_id) {
                    self.display_ids.entry(id).or_default().push(idx);
                }
            }
            "update_display_data" => {
                let Ok(d) = m.content_as::<content::DisplayData>() else {
                    return;
                };
                let Some(id) = d.transient.and_then(|t| t.display_id) else {
                    return;
                };
                if let Some(indices) = self.display_ids.get(&id) {
                    for &i in indices {
                        if let Some(Output::DisplayData(o)) = self.outputs.get_mut(i) {
                            o.data = bundle(d.data.clone());
                            o.metadata = d.metadata.clone();
                        }
                    }
                }
            }
            "execute_result" => {
                let Ok(r) = m.content_as::<content::ExecuteResult>() else {
                    return;
                };
                self.before_output();
                self.outputs
                    .push(Output::ExecuteResult(ExecuteResultOutput {
                        execution_count: r.execution_count,
                        data: bundle(r.data),
                        metadata: r.metadata,
                        extra: Default::default(),
                    }));
            }
            "error" => {
                let Ok(e) = m.content_as::<content::ErrorContent>() else {
                    return;
                };
                self.before_output();
                self.outputs.push(Output::Error(ErrorOutput {
                    ename: e.ename,
                    evalue: e.evalue,
                    traceback: e.traceback,
                    extra: Default::default(),
                }));
            }
            _ => {}
        }
    }

    fn before_output(&mut self) {
        if self.clear_pending {
            self.clear();
        }
    }

    fn clear(&mut self) {
        self.outputs.clear();
        self.display_ids.clear();
        self.clear_pending = false;
    }

    pub fn finish(self) -> Vec<Output> {
        self.outputs
    }
}

fn bundle(m: serde_json::Map<String, serde_json::Value>) -> MimeBundle {
    let mut b = MimeBundle::new();
    for (k, v) in m {
        b.insert(k, v);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcore_kernel_host::testing::{FakeKernel, ReplyOrder};
    use jcore_kernel_host::{in_process_pair, protocol::Message};

    fn msg(t: &str, c: serde_json::Value) -> Message {
        Message::new(t, "s", "u", &c)
    }

    #[test]
    fn builder_merges_streams_and_handles_clear_and_display_id() {
        let mut b = OutputBuilder::new(true);
        b.push(&msg(
            "stream",
            serde_json::json!({"name":"stdout","text":"a"}),
        ));
        b.push(&msg(
            "stream",
            serde_json::json!({"name":"stdout","text":"b\n"}),
        ));
        b.push(&msg(
            "stream",
            serde_json::json!({"name":"stderr","text":"e"}),
        ));
        b.push(&msg("display_data", serde_json::json!({"data":{"text/plain":"v1"},"metadata":{},"transient":{"display_id":"d1"}})));
        b.push(&msg("update_display_data", serde_json::json!({"data":{"text/plain":"v2"},"metadata":{},"transient":{"display_id":"d1"}})));
        let out = b.finish();
        assert_eq!(out.len(), 3);
        let Output::Stream(s) = &out[0] else { panic!() };
        assert_eq!((s.name.as_str(), s.text.as_str()), ("stdout", "ab\n"));
        let Output::DisplayData(d) = &out[2] else {
            panic!()
        };
        assert_eq!(d.data.get_str("text/plain"), Some("v2"));

        let mut b = OutputBuilder::new(true);
        b.push(&msg(
            "stream",
            serde_json::json!({"name":"stdout","text":"old"}),
        ));
        b.push(&msg("clear_output", serde_json::json!({"wait":true})));
        assert_eq!(
            b.outputs.len(),
            1,
            "wait=true keeps old output until the next arrives"
        );
        b.push(&msg(
            "execute_result",
            serde_json::json!({"execution_count":1,"data":{"text/plain":"new"},"metadata":{}}),
        ));
        let out = b.finish();
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Output::ExecuteResult(_)));
    }

    #[tokio::test]
    async fn executes_a_notebook_on_the_fake_kernel_and_stops_on_error() {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        let s = KernelSession::start(host, None);
        s.nudge(Duration::from_secs(5)).await.unwrap();

        let mut nb: Notebook = Notebook::from_str(r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
            {"cell_type":"markdown","id":"m","source":"# hi","metadata":{}},
            {"cell_type":"code","id":"a","source":"one","metadata":{},"outputs":[{"output_type":"stream","name":"stdout","text":"stale"}],"execution_count":9},
            {"cell_type":"code","id":"b","source":"raise","metadata":{},"outputs":[],"execution_count":null},
            {"cell_type":"code","id":"c","source":"three","metadata":{},"outputs":[],"execution_count":null}
        ]}"##).unwrap();

        let err = execute_notebook(&s, &mut nb, &Options::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CellFailed { index: 2, .. }));
        let Cell::Code(a) = &nb.cells[1] else {
            panic!()
        };
        assert_eq!(a.execution_count, Some(1));
        assert_eq!(a.outputs.len(), 2);
        let Output::ExecuteResult(r) = &a.outputs[1] else {
            panic!()
        };
        assert_eq!(r.data.get_str("text/plain"), Some("ONE"));
        let Cell::Code(b) = &nb.cells[2] else {
            panic!()
        };
        assert!(matches!(b.outputs.last(), Some(Output::Error(e)) if e.ename == "FakeError"));
        let Cell::Code(c) = &nb.cells[3] else {
            panic!()
        };
        assert!(
            c.outputs.is_empty() && c.execution_count.is_none(),
            "not run after the failure"
        );

        let report = execute_notebook(
            &s,
            &mut nb,
            &Options {
                allow_errors: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (report.executed, report.errored, report.stopped_at),
            (3, vec![2], None)
        );
        let Cell::Code(c) = &nb.cells[3] else {
            panic!()
        };
        assert_eq!(c.execution_count, Some(5));
    }
}
