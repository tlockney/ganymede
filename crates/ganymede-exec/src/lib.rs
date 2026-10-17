//! Run a notebook's code cells on a [`KernelSession`] and store what comes
//! back as nbformat outputs. Language-neutral: it only translates iopub
//! messages into output records.
//!
//! The defaults reproduce `nbclient` (what `jupyter execute` and
//! `nbconvert --execute` run), so a notebook executed here and there differs
//! only in timestamps and whatever the kernel itself makes nondeterministic:
//! - `metadata.language_info` comes from `kernel_info_reply`.
//! - Cells whose source is blank, and cells tagged `skip-execution`, are
//!   left untouched.
//! - `execution_count` counts the cells this run executed.
//! - `metadata.execution` records the busy, execute_input, reply, and idle
//!   timestamps from the kernel's message headers.
//! - `clear_output` with `wait` clears when the next output arrives.
//! - A `display_id` update rewrites every output with that id, in any cell
//!   executed so far.
//! - Streams are not merged unless [`Options::coalesce_streams`].
//! - Widget state from `jupyter.widget` comms lands in
//!   `metadata.widgets`.
//! - An error reply stops execution unless errors are allowed for the
//!   run, for the error name, or for the cell (tag `raises-exception`).

use base64::Engine as _;
use ganymede_kernel_host::protocol::content::{self, ExecuteRequest};
use ganymede_kernel_host::protocol::Message;
use ganymede_kernel_host::KernelSession;
use ganymede_nbformat::{
    Cell, CodeCell, DisplayDataOutput, ErrorOutput, ExecuteResultOutput, MimeBundle,
    MultilineString, Notebook, Output, StreamOutput,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kernel: {0}")]
    Kernel(#[from] ganymede_kernel_host::Error),
    #[error("cell {index} failed: {ename}: {evalue}")]
    CellFailed {
        index: usize,
        ename: String,
        evalue: String,
    },
    #[error("cell {index} timed out after {timeout:?}")]
    CellTimeout { index: usize, timeout: Duration },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stands in for "no timeout"; long enough, short enough not to overflow
/// an `Instant`.
const FOREVER: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

#[derive(Debug, Clone)]
pub struct Options {
    /// Per cell. `None` waits forever. Cell metadata `execution.timeout`
    /// (seconds) overrides it, as in `nbclient`.
    pub timeout: Option<Duration>,
    pub allow_errors: bool,
    /// Error names (`ename`) that do not stop execution.
    pub allow_error_names: Vec<String>,
    /// Stop on any error, ignoring the three allowances.
    pub force_raise_errors: bool,
    pub skip_cells_with_tag: String,
    /// Merge streams with the same name into one output per cell, and
    /// resolve carriage returns, as `nbclient`'s option of the same name.
    pub coalesce_streams: bool,
    /// Record `metadata.execution` timings on each executed cell.
    pub record_timing: bool,
    /// Save `jupyter.widget` comm state in `metadata.widgets`.
    pub store_widget_state: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: None,
            allow_errors: false,
            allow_error_names: Vec::new(),
            force_raise_errors: false,
            skip_cells_with_tag: "skip-execution".into(),
            coalesce_streams: false,
            record_timing: true,
            store_widget_state: true,
        }
    }
}

/// What happened, cell by cell.
#[derive(Debug, Default)]
pub struct Report {
    pub executed: usize,
    /// Indices of cells whose reply was an error, allowed or not.
    pub errored: Vec<usize>,
    /// Index of the cell that stopped execution, if any.
    pub stopped_at: Option<usize>,
}

/// Progress events for a caller that shows them.
#[derive(Debug)]
pub enum Event<'a> {
    CellStarted {
        index: usize,
        cell: &'a CodeCell,
    },
    CellFinished {
        index: usize,
        cell: &'a CodeCell,
        ok: bool,
    },
}

/// Execute every code cell in order, writing outputs into `nb`.
pub async fn execute_notebook(
    session: &KernelSession,
    nb: &mut Notebook,
    opts: &Options,
) -> Result<Report> {
    execute_notebook_with(session, nb, opts, &mut |_| {}).await
}

/// [`execute_notebook`], reporting each cell to `on_event`.
pub async fn execute_notebook_with(
    session: &KernelSession,
    nb: &mut Notebook,
    opts: &Options,
    on_event: &mut dyn FnMut(Event<'_>),
) -> Result<Report> {
    let mut run = NotebookRun::new(opts.clone());
    if let Some(info) = session.kernel_info_json() {
        if let Some(li) = info.get("language_info") {
            nb.metadata.insert("language_info".into(), li.clone());
        }
    }
    let mut report = Report::default();
    let mut outcome = Ok(());
    for index in 0..nb.cells.len() {
        let Cell::Code(cell) = &nb.cells[index] else {
            continue;
        };
        if !run.should_execute(cell) {
            continue;
        }
        on_event(Event::CellStarted { index, cell });
        let result = run.execute_cell(session, &mut nb.cells, index).await;
        let Cell::Code(cell) = &nb.cells[index] else {
            unreachable!()
        };
        match result {
            Ok(status) => {
                report.executed += 1;
                let ok = status == CellStatus::Ok;
                on_event(Event::CellFinished { index, cell, ok });
                if let CellStatus::Error { allowed, .. } = &status {
                    report.errored.push(index);
                    if !allowed {
                        report.stopped_at = Some(index);
                        let (ename, evalue) = status.error_names();
                        outcome = Err(Error::CellFailed {
                            index,
                            ename,
                            evalue,
                        });
                        break;
                    }
                }
            }
            Err(e) => {
                report.stopped_at = Some(index);
                outcome = Err(e);
                break;
            }
        }
    }
    run.write_widget_state(nb);
    outcome.map(|()| report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellStatus {
    Ok,
    Error {
        ename: String,
        evalue: String,
        allowed: bool,
    },
}

impl CellStatus {
    fn error_names(&self) -> (String, String) {
        match self {
            CellStatus::Ok => Default::default(),
            CellStatus::Error { ename, evalue, .. } => (ename.clone(), evalue.clone()),
        }
    }
}

/// State that spans cells: the execution counter, display ids, widget
/// state. One per notebook execution.
pub struct NotebookRun {
    opts: Options,
    executed: u64,
    /// display_id -> cell index -> output indices in that cell.
    display_ids: HashMap<String, BTreeMap<usize, Vec<usize>>>,
    widget_state: BTreeMap<String, Map<String, Value>>,
    /// comm id -> buffer path (as JSON) -> encoded buffer.
    widget_buffers: BTreeMap<String, BTreeMap<String, Value>>,
}

impl NotebookRun {
    pub fn new(opts: Options) -> Self {
        NotebookRun {
            opts,
            executed: 0,
            display_ids: HashMap::new(),
            widget_state: BTreeMap::new(),
            widget_buffers: BTreeMap::new(),
        }
    }

    /// False for blank cells and cells carrying the skip tag.
    pub fn should_execute(&self, cell: &CodeCell) -> bool {
        !cell.source.as_str().trim().is_empty()
            && !has_tag(&cell.metadata, &self.opts.skip_cells_with_tag)
    }

    /// Execute `cells[index]`, which must be a code cell.
    pub async fn execute_cell(
        &mut self,
        session: &KernelSession,
        cells: &mut [Cell],
        index: usize,
    ) -> Result<CellStatus> {
        let Cell::Code(cell) = &mut cells[index] else {
            panic!("cell {index} is not a code cell");
        };
        let allows_errors = !self.opts.force_raise_errors
            && (self.opts.allow_errors || has_tag(&cell.metadata, "raises-exception"));
        if self.opts.record_timing {
            cell.metadata.insert("execution".into(), json!({}));
        }
        let mut req = ExecuteRequest::new(cell.source.as_str().to_string());
        req.allow_stdin = false;
        req.stop_on_error = !allows_errors;
        let timeout = cell_timeout(cell).or(self.opts.timeout);
        cell.outputs.clear();
        self.executed += 1;
        let execution_count = self.executed;
        let exec = session
            .execute(req, timeout.unwrap_or(FOREVER))
            .await
            .map_err(|e| match e {
                ganymede_kernel_host::Error::Timeout(_) => Error::CellTimeout {
                    index,
                    timeout: timeout.unwrap_or_default(),
                },
                e => Error::Kernel(e),
            })?;

        // Messages in arrival order, as nbclient handles them: the reply's
        // place among iopub messages only matters for timing.
        let mut clear_pending = false;
        for m in exec.status.iter().chain(exec.outputs.iter()) {
            self.record_timing(cells, index, m);
        }
        for m in &exec.outputs {
            self.process(cells, index, m, &mut clear_pending);
        }
        let Cell::Code(cell) = &mut cells[index] else {
            unreachable!()
        };
        if self.opts.record_timing {
            if let Some(t) = cell
                .metadata
                .get_mut("execution")
                .and_then(Value::as_object_mut)
            {
                t.insert(
                    "shell.execute_reply".into(),
                    Value::String(timestamp(&exec.reply)),
                );
            }
        }
        cell.execution_count = Some(execution_count);
        if self.opts.coalesce_streams {
            coalesce(&mut cell.outputs);
        }

        let reply: content::ExecuteReply = exec
            .reply
            .content_as()
            .map_err(ganymede_kernel_host::Error::from)?;
        if reply.status == content::ReplyStatus::Ok {
            return Ok(CellStatus::Ok);
        }
        let ename = exec.reply.content["ename"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let evalue = exec.reply.content["evalue"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let allowed = !self.opts.force_raise_errors
            && (allows_errors || self.opts.allow_error_names.contains(&ename));
        Ok(CellStatus::Error {
            ename,
            evalue,
            allowed,
        })
    }

    fn record_timing(&self, cells: &mut [Cell], index: usize, m: &Message) {
        if !self.opts.record_timing {
            return;
        }
        let key = match (m.msg_type(), m.content["execution_state"].as_str()) {
            ("status", Some("busy")) => "iopub.status.busy",
            ("status", Some("idle")) => "iopub.status.idle",
            ("execute_input", _) => "iopub.execute_input",
            _ => return,
        };
        if let Cell::Code(c) = &mut cells[index] {
            if let Some(t) = c
                .metadata
                .get_mut("execution")
                .and_then(Value::as_object_mut)
            {
                t.insert(key.into(), Value::String(timestamp(m)));
            }
        }
    }

    fn process(&mut self, cells: &mut [Cell], index: usize, m: &Message, clear_pending: &mut bool) {
        let msg_type = m.msg_type();
        let display_id = m
            .content
            .get("transient")
            .and_then(|t| t.get("display_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(id) = &display_id {
            if matches!(
                msg_type,
                "execute_result" | "display_data" | "update_display_data"
            ) {
                self.update_display(cells, id, m);
            }
        }
        match msg_type {
            "clear_output" => {
                if m.content["wait"].as_bool().unwrap_or(false) {
                    *clear_pending = true;
                } else {
                    self.clear(cells, index);
                }
            }
            t if t.starts_with("comm") => self.comm(m),
            "execute_input" | "update_display_data" => {}
            _ => {
                let Some(out) = output_from_msg(m) else {
                    return;
                };
                if *clear_pending {
                    self.clear(cells, index);
                    *clear_pending = false;
                }
                let Cell::Code(cell) = &mut cells[index] else {
                    return;
                };
                if let Some(id) = display_id {
                    self.display_ids
                        .entry(id)
                        .or_default()
                        .entry(index)
                        .or_default()
                        .push(cell.outputs.len());
                }
                cell.outputs.push(out);
            }
        }
    }

    fn clear(&mut self, cells: &mut [Cell], index: usize) {
        if let Cell::Code(cell) = &mut cells[index] {
            cell.outputs.clear();
        }
        for map in self.display_ids.values_mut() {
            if let Some(v) = map.get_mut(&index) {
                v.clear();
            }
        }
    }

    fn update_display(&self, cells: &mut [Cell], id: &str, m: &Message) {
        let Some(map) = self.display_ids.get(id) else {
            return;
        };
        let data = bundle(&m.content["data"]);
        let metadata = m.content["metadata"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (&ci, outs) in map {
            let Some(Cell::Code(cell)) = cells.get_mut(ci) else {
                continue;
            };
            for &oi in outs {
                match cell.outputs.get_mut(oi) {
                    Some(Output::DisplayData(o)) => {
                        o.data = data.clone();
                        o.metadata = metadata.clone();
                    }
                    Some(Output::ExecuteResult(o)) => {
                        o.data = data.clone();
                        o.metadata = metadata.clone();
                    }
                    _ => {}
                }
            }
        }
    }

    fn comm(&mut self, m: &Message) {
        if !self.opts.store_widget_state {
            return;
        }
        let Some(data) = m.content.get("data").and_then(Value::as_object) else {
            return;
        };
        let Some(state) = data.get("state").and_then(Value::as_object) else {
            return;
        };
        let comm_id = m.content["comm_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let entry = self.widget_state.entry(comm_id.clone()).or_default();
        for (k, v) in state {
            entry.insert(k.clone(), v.clone());
        }
        if let Some(paths) = data.get("buffer_paths").and_then(Value::as_array) {
            if paths.is_empty() {
                return;
            }
            let buffers = self.widget_buffers.entry(comm_id).or_default();
            for (path, buf) in paths.iter().zip(&m.buffers) {
                buffers.insert(
                    path.to_string(),
                    json!({
                        "data": base64::engine::general_purpose::STANDARD.encode(buf),
                        "encoding": "base64",
                        "path": path,
                    }),
                );
            }
        }
    }

    /// Store collected widget state in `metadata.widgets`, in the
    /// `@jupyter-widgets/schema` shape.
    pub fn write_widget_state(&self, nb: &mut Notebook) {
        if self.widget_state.is_empty() {
            return;
        }
        let mut state = Map::new();
        for (id, s) in &self.widget_state {
            if !s.contains_key("_model_name") {
                continue;
            }
            let mut w = json!({
                "model_name": s.get("_model_name"),
                "model_module": s.get("_model_module"),
                "model_module_version": s.get("_model_module_version"),
                "state": s,
            });
            if let Some(b) = self.widget_buffers.get(id).filter(|b| !b.is_empty()) {
                w["buffers"] = Value::Array(b.values().cloned().collect());
            }
            state.insert(id.clone(), w);
        }
        nb.metadata.insert(
            "widgets".into(),
            json!({
                "application/vnd.jupyter.widget-state+json": {
                    "state": state,
                    "version_major": 2,
                    "version_minor": 0,
                }
            }),
        );
    }
}

/// Execute one cell outside a notebook run. Its outputs replace the cell's.
pub async fn execute_cell(
    session: &KernelSession,
    cell: &mut CodeCell,
    opts: &Options,
) -> Result<CellStatus> {
    let mut run = NotebookRun::new(opts.clone());
    let mut cells = vec![Cell::Code(std::mem::take(cell))];
    let r = run.execute_cell(session, &mut cells, 0).await;
    if let Some(Cell::Code(c)) = cells.pop() {
        *cell = c;
    }
    r
}

/// The outputs one cell shows for these iopub messages, built by the same
/// rules as a notebook run: clears, display updates, no stream merging.
pub fn outputs_from_messages<'a>(messages: impl IntoIterator<Item = &'a Message>) -> Vec<Output> {
    let mut run = NotebookRun::new(Options {
        record_timing: false,
        store_widget_state: false,
        ..Default::default()
    });
    let mut cells = vec![Cell::Code(CodeCell::default())];
    let mut clear_pending = false;
    for m in messages {
        run.process(&mut cells, 0, m, &mut clear_pending);
    }
    match cells.pop() {
        Some(Cell::Code(c)) => c.outputs,
        _ => Vec::new(),
    }
}

fn has_tag(metadata: &Map<String, Value>, tag: &str) -> bool {
    metadata
        .get("tags")
        .and_then(Value::as_array)
        .is_some_and(|t| t.iter().any(|v| v.as_str() == Some(tag)))
}

fn cell_timeout(cell: &CodeCell) -> Option<Duration> {
    cell.metadata
        .get("execution")
        .and_then(|e| e.get("timeout"))
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0)
        .map(Duration::from_secs_f64)
}

/// A message's header date in `nbclient`'s format: microseconds and `Z`,
/// with the wall time as the kernel wrote it.
pub fn timestamp(m: &Message) -> String {
    let d = &m.header.date;
    let parsed = chrono::DateTime::parse_from_rfc3339(d)
        .map(|t| t.naive_local())
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S%.f"));
    match parsed {
        Ok(t) => t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
        Err(_) => d.clone(),
    }
}

/// nbformat's `output_from_msg`: the output record for an iopub message,
/// or `None` for messages that are not outputs.
pub fn output_from_msg(m: &Message) -> Option<Output> {
    let c = &m.content;
    let meta = || c["metadata"].as_object().cloned().unwrap_or_default();
    Some(match m.msg_type() {
        "stream" => Output::Stream(StreamOutput {
            name: c["name"].as_str()?.to_string(),
            text: MultilineString::new(c["text"].as_str()?),
            extra: Default::default(),
        }),
        "display_data" => Output::DisplayData(DisplayDataOutput {
            data: bundle(&c["data"]),
            metadata: meta(),
            extra: Default::default(),
        }),
        "execute_result" => Output::ExecuteResult(ExecuteResultOutput {
            execution_count: c["execution_count"].as_u64(),
            data: bundle(&c["data"]),
            metadata: meta(),
            extra: Default::default(),
        }),
        "error" => Output::Error(ErrorOutput {
            ename: c["ename"].as_str().unwrap_or_default().to_string(),
            evalue: c["evalue"].as_str().unwrap_or_default().to_string(),
            traceback: c["traceback"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            extra: Default::default(),
        }),
        _ => return None,
    })
}

fn bundle(v: &Value) -> MimeBundle {
    let mut b = MimeBundle::new();
    if let Some(m) = v.as_object() {
        for (k, v) in m {
            b.insert(k.clone(), v.clone());
        }
    }
    b
}

/// `nbclient`'s `coalesce_streams`: one output per stream name at the
/// position of its first chunk, carriage returns resolved, and stdout
/// moved ahead of an immediately preceding stderr.
fn coalesce(outputs: &mut Vec<Output>) {
    let mut merged: Vec<Output> = Vec::new();
    let mut first: HashMap<String, usize> = HashMap::new();
    for o in outputs.drain(..) {
        if let Output::Stream(s) = &o {
            if let Some(&i) = first.get(&s.name) {
                if let Output::Stream(t) = &mut merged[i] {
                    t.text.as_mut_string().push_str(s.text.as_str());
                }
                continue;
            }
            first.insert(s.name.clone(), merged.len());
        }
        merged.push(o);
    }
    for &i in first.values() {
        if let Output::Stream(s) = &mut merged[i] {
            *s.text.as_mut_string() = resolve_carriage_returns(s.text.as_str());
        }
    }
    // nbclient also tries to drop characters before backspaces, but its
    // loop condition is false on entry, so it never does. Neither do we.
    let mut i = 0;
    while i + 1 < merged.len() {
        let swap = matches!((&merged[i], &merged[i + 1]),
            (Output::Stream(a), Output::Stream(b)) if a.name == "stderr" && b.name == "stdout");
        if swap {
            let out = merged.remove(i + 1);
            merged.insert(i, out);
        }
        i += 1;
    }
    *outputs = merged;
}

/// nbclient's `_RGX_CARRIAGERETURN` substitution: on each line, drop
/// everything through the last carriage return that has text after it.
fn resolve_carriage_returns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let b = line.as_bytes();
        match (0..b.len().saturating_sub(1))
            .rev()
            .find(|&i| b[i] == b'\r')
        {
            Some(i) => out.push_str(&line[i + 1..]),
            None => out.push_str(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
    use ganymede_kernel_host::{in_process_pair, protocol::Message};

    fn msg(t: &str, c: Value) -> Message {
        Message::new(t, "s", "u", &c)
    }

    fn code_cell(src: &str) -> Cell {
        Cell::Code(CodeCell {
            source: MultilineString::new(src),
            ..Default::default()
        })
    }

    fn outputs(cells: &[Cell], i: usize) -> &[Output] {
        let Cell::Code(c) = &cells[i] else { panic!() };
        &c.outputs
    }

    #[test]
    fn display_ids_update_across_cells_and_clear_wait_defers() {
        let mut run = NotebookRun::new(Options::default());
        let mut cells = vec![code_cell("a"), code_cell("b")];
        let mut pending = false;
        run.process(
            &mut cells,
            0,
            &msg("stream", json!({"name":"stdout","text":"a"})),
            &mut pending,
        );
        run.process(
            &mut cells,
            0,
            &msg("stream", json!({"name":"stdout","text":"b\n"})),
            &mut pending,
        );
        run.process(
            &mut cells,
            0,
            &msg(
                "display_data",
                json!({"data":{"text/plain":"v1"},"metadata":{},"transient":{"display_id":"d1"}}),
            ),
            &mut pending,
        );
        assert_eq!(outputs(&cells, 0).len(), 3, "streams are not merged");

        // A later cell updates the first cell's display, and shows its own.
        run.process(
            &mut cells,
            1,
            &msg(
                "display_data",
                json!({"data":{"text/plain":"v2"},"metadata":{},"transient":{"display_id":"d1"}}),
            ),
            &mut pending,
        );
        run.process(
            &mut cells,
            1,
            &msg(
                "update_display_data",
                json!({"data":{"text/plain":"v3"},"metadata":{},"transient":{"display_id":"d1"}}),
            ),
            &mut pending,
        );
        for i in [0, 1] {
            let Some(Output::DisplayData(d)) = outputs(&cells, i).last() else {
                panic!()
            };
            assert_eq!(d.data.get_str("text/plain"), Some("v3"));
        }

        run.process(
            &mut cells,
            1,
            &msg("clear_output", json!({"wait":true})),
            &mut pending,
        );
        assert_eq!(outputs(&cells, 1).len(), 1, "wait=true keeps old output");
        run.process(
            &mut cells,
            1,
            &msg(
                "execute_result",
                json!({"execution_count":2,"data":{"text/plain":"new"},"metadata":{}}),
            ),
            &mut pending,
        );
        assert!(matches!(outputs(&cells, 1), [Output::ExecuteResult(_)]));
    }

    #[test]
    fn coalesce_matches_nbclient() {
        let s = |n: &str, t: &str| {
            Output::Stream(StreamOutput {
                name: n.into(),
                text: MultilineString::new(t),
                extra: Default::default(),
            })
        };
        let mut outs = vec![
            s("stderr", "e1"),
            s("stdout", "10%\r50%\r100%\n"),
            s("stderr", "e2"),
            s("stdout", "done\r\n"),
        ];
        coalesce(&mut outs);
        let texts: Vec<_> = outs
            .iter()
            .map(|o| match o {
                Output::Stream(s) => (s.name.as_str(), s.text.as_str().to_string()),
                _ => panic!(),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                ("stdout", "100%\ndone\r\n".to_string()),
                ("stderr", "e1e2".to_string())
            ]
        );
    }

    #[test]
    fn timestamps_use_nbclient_format() {
        let mut m = msg("status", json!({}));
        m.header.date = "2026-10-17T16:41:06.123Z".into();
        assert_eq!(timestamp(&m), "2026-10-17T16:41:06.123000Z");
        m.header.date = "2026-10-17T16:41:06.123456+00:00".into();
        assert_eq!(timestamp(&m), "2026-10-17T16:41:06.123456Z");
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
            {"cell_type":"code","id":"e","source":"  ","metadata":{},"outputs":[{"output_type":"stream","name":"stdout","text":"kept"}],"execution_count":3},
            {"cell_type":"code","id":"s","source":"skipped","metadata":{"tags":["skip-execution"]},"outputs":[],"execution_count":null},
            {"cell_type":"code","id":"b","source":"raise","metadata":{},"outputs":[],"execution_count":null},
            {"cell_type":"code","id":"c","source":"three","metadata":{},"outputs":[],"execution_count":null}
        ]}"##).unwrap();

        let err = execute_notebook(&s, &mut nb, &Options::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CellFailed { index: 4, .. }));
        assert_eq!(nb.metadata["language_info"]["name"], "fake");
        let Cell::Code(a) = &nb.cells[1] else {
            panic!()
        };
        assert_eq!(a.execution_count, Some(1));
        assert_eq!(a.outputs.len(), 2);
        let timing = a.metadata["execution"].as_object().unwrap();
        let keys: Vec<_> = timing.keys().map(String::as_str).collect();
        for k in [
            "iopub.status.busy",
            "iopub.execute_input",
            "shell.execute_reply",
            "iopub.status.idle",
        ] {
            assert!(keys.contains(&k), "{k} missing from {keys:?}");
        }
        let Output::ExecuteResult(r) = &a.outputs[1] else {
            panic!()
        };
        assert_eq!(r.data.get_str("text/plain"), Some("ONE"));
        let Cell::Code(e) = &nb.cells[2] else {
            panic!()
        };
        assert_eq!(
            (e.execution_count, e.outputs.len()),
            (Some(3), 1),
            "blank cells are left alone"
        );
        let Cell::Code(b) = &nb.cells[4] else {
            panic!()
        };
        assert_eq!(b.execution_count, Some(2), "counts cells this run executed");
        assert!(matches!(b.outputs.last(), Some(Output::Error(e)) if e.ename == "FakeError"));
        let Cell::Code(c) = &nb.cells[5] else {
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
                allow_error_names: vec!["FakeError".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (report.executed, report.errored, report.stopped_at),
            (3, vec![4], None)
        );
        let Cell::Code(c) = &nb.cells[5] else {
            panic!()
        };
        assert_eq!(c.execution_count, Some(3));
    }
}
