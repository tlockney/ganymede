//! `ganymede console`: a REPL on any kernel. With a terminal it has line
//! editing, history, tab completion from the kernel, and multi-line input
//! by the kernel's `is_complete_reply`. Without one it reads stdin and runs
//! each complete chunk, so it can be scripted.

use crate::render::Terminal;
use anyhow::Result;
use ganymede_engine::{ExecuteOptions, IsComplete, Kernel};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Editor, Helper};
use std::io::{BufRead, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;

const HELP: &str = "\
Enter code to run it. Multi-line input continues until the kernel says it
is complete. Tab completes from the kernel. Ctrl-C interrupts a running
cell or clears the line; Ctrl-D or :quit exits.
  :restart   restart the kernel (state is lost)
  :help      this text";

/// Run the console until EOF or `:quit`.
pub async fn run(kernel: Arc<Kernel>) -> Result<()> {
    // Ctrl-C while a cell runs interrupts the kernel. At the prompt the
    // line editor reads it as a key instead.
    let k = kernel.clone();
    let interrupter = tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if let Err(e) = k.interrupt().await {
                eprintln!("interrupt failed: {e}");
            }
        }
    });
    let handle = Handle::current();
    let result = if std::io::stdin().is_terminal() {
        let info = kernel.info();
        if !info.banner.is_empty() {
            eprintln!("{}", info.banner.trim_end());
        }
        eprintln!(
            "{} {} ({}). :help for help.",
            info.implementation, info.implementation_version, info.language_info.name
        );
        tokio::task::spawn_blocking(move || interactive(kernel, handle)).await?
    } else {
        tokio::task::spawn_blocking(move || piped(kernel, handle)).await?
    };
    interrupter.abort();
    result
}

fn execute(kernel: &Kernel, handle: &Handle, code: &str, stdin_allowed: bool) -> bool {
    let opts = ExecuteOptions {
        allow_stdin: stdin_allowed,
        stop_on_error: false,
        ..Default::default()
    };
    let mut term = Terminal { stdin_allowed };
    match handle.block_on(kernel.execute_with(code, &opts, &mut term)) {
        Ok(r) => r.is_ok(),
        Err(e) => {
            eprintln!("error: {e}");
            false
        }
    }
}

/// Set once a kernel fails to answer `is_complete_request` in time, so
/// every later line does not wait for it too.
static NO_IS_COMPLETE: AtomicBool = AtomicBool::new(false);

fn is_complete(kernel: &Kernel, handle: &Handle, code: &str) -> IsComplete {
    if NO_IS_COMPLETE.load(Ordering::Relaxed) {
        return IsComplete::Unknown;
    }
    let ask = tokio::time::timeout(Duration::from_secs(5), kernel.is_complete(code));
    match handle.block_on(ask) {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => IsComplete::Unknown,
        Err(_) => {
            NO_IS_COMPLETE.store(true, Ordering::Relaxed);
            IsComplete::Unknown
        }
    }
}

fn piped(kernel: Arc<Kernel>, handle: Handle) -> Result<()> {
    let mut buf = String::new();
    let mut failed = false;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        buf.push_str(&line);
        buf.push('\n');
        if buf.trim().is_empty() {
            buf.clear();
            continue;
        }
        // A kernel that cannot judge (`unknown`) runs at blank lines.
        let ready = match is_complete(&kernel, &handle, &buf) {
            IsComplete::Complete | IsComplete::Invalid => true,
            IsComplete::Incomplete { .. } => false,
            IsComplete::Unknown => line.trim().is_empty(),
        };
        if ready {
            failed |= !execute(&kernel, &handle, &buf, false);
            buf.clear();
        }
    }
    if !buf.trim().is_empty() {
        failed |= !execute(&kernel, &handle, &buf, false);
    }
    if failed {
        anyhow::bail!("a cell failed");
    }
    Ok(())
}

/// Where the interactive console keeps its line history.
pub fn history_path() -> Option<std::path::PathBuf> {
    ganymede_engine::host::kernelspec::jupyter_user_data_dir()
        .map(|d| d.join("ganymede-console-history"))
}

fn interactive(kernel: Arc<Kernel>, handle: Handle) -> Result<()> {
    let helper = KernelHelper {
        kernel: kernel.clone(),
        handle: handle.clone(),
    };
    let config = rustyline::Config::builder()
        .auto_add_history(true)
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl: Editor<KernelHelper, DefaultHistory> = Editor::with_config(config)?;
    rl.set_helper(Some(helper));
    let history = history_path();
    if let Some(h) = &history {
        let _ = rl.load_history(h);
    }
    let mut n = 1u64;
    loop {
        match rl.readline(&format!("In [{n}]: ")) {
            Ok(line) => match line.trim() {
                "" => {}
                ":quit" | ":q" => break,
                ":help" => eprintln!("{HELP}"),
                ":restart" => match handle.block_on(kernel.restart()) {
                    Ok(()) => {
                        n = 1;
                        eprintln!("kernel restarted");
                    }
                    Err(e) => eprintln!("restart failed: {e}"),
                },
                _ => {
                    execute(&kernel, &handle, &line, true);
                    n += 1;
                }
            },
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(h) = &history {
        if let Some(dir) = h.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = rl.save_history(h);
    }
    Ok(())
}

struct KernelHelper {
    kernel: Arc<Kernel>,
    handle: Handle,
}

impl Completer for KernelHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // The protocol counts the cursor in Unicode code points.
        let cursor = line[..pos].chars().count();
        let Ok(r) = self.handle.block_on(self.kernel.complete(line, cursor)) else {
            return Ok((pos, vec![]));
        };
        let start_chars = r["cursor_start"].as_u64().unwrap_or(cursor as u64) as usize;
        let start = line
            .char_indices()
            .nth(start_chars)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let matches = r["matches"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.as_str())
                    .map(|m| Pair {
                        display: m.to_string(),
                        replacement: m.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((start, matches))
    }
}

impl Validator for KernelHelper {
    fn validate(&self, ctx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        let input = ctx.input();
        if input.trim_start().starts_with(':') || input.trim().is_empty() {
            return Ok(ValidationResult::Valid(None));
        }
        // A trailing blank line always submits, so a kernel that keeps
        // saying "incomplete" cannot trap the user.
        if input.ends_with('\n') {
            return Ok(ValidationResult::Valid(None));
        }
        Ok(match is_complete(&self.kernel, &self.handle, input) {
            IsComplete::Incomplete { .. } => ValidationResult::Incomplete,
            _ => ValidationResult::Valid(None),
        })
    }
}

impl Hinter for KernelHelper {
    type Hint = String;
}

impl Highlighter for KernelHelper {}

impl Helper for KernelHelper {}
