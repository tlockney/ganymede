//! Showing kernel output in a terminal, or as JSON for scripts.

use ganymede_engine::nbformat::Output;
use ganymede_engine::protocol::Message;
use ganymede_engine::{ExecuteResult, Handler};
use std::io::{IsTerminal, Write};

/// Rich types a terminal can show as text, in order of preference.
const TEXT_MIMES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/latex",
    "application/json",
];

/// Prints iopub output as it streams in: streams to the matching file
/// descriptor, results and displays as text, errors as tracebacks on
/// stderr. Answers input requests from the terminal.
pub struct Terminal {
    pub stdin_allowed: bool,
}

impl Handler for Terminal {
    fn on_iopub(&mut self, m: &Message) {
        let c = &m.content;
        match m.msg_type() {
            "stream" => {
                let text = c["text"].as_str().unwrap_or_default();
                if c["name"] == "stderr" {
                    eprint!("{text}");
                    let _ = std::io::stderr().flush();
                } else {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
            }
            "execute_result" | "display_data" => print_bundle(&c["data"]),
            "error" => {
                let tb: Vec<&str> = c["traceback"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if tb.is_empty() {
                    eprintln!(
                        "{}: {}",
                        c["ename"].as_str().unwrap_or_default(),
                        c["evalue"].as_str().unwrap_or_default()
                    );
                } else {
                    for line in tb {
                        eprintln!("{}", for_stderr(line));
                    }
                }
            }
            _ => {}
        }
    }

    fn on_input(&mut self, prompt: &str, password: bool) -> Option<String> {
        if !self.stdin_allowed {
            return None;
        }
        // The handler runs on the runtime; reading a line blocks.
        tokio::task::block_in_place(|| {
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
            let _echo = password.then(NoEcho::new).flatten();
            let mut line = String::new();
            let r = std::io::stdin().read_line(&mut line);
            if _echo.is_some() {
                eprintln!();
            }
            // End of input is an empty reply, as `jupyter console` sends.
            r.ok()?;
            Some(line.trim_end_matches(['\n', '\r']).to_string())
        })
    }
}

/// Turns off terminal echo for a password until dropped, with `stty`,
/// when stdin is a terminal. `None` where that is not possible, and the
/// password is echoed.
struct NoEcho;

impl NoEcho {
    fn new() -> Option<NoEcho> {
        if !cfg!(unix) || !std::io::stdin().is_terminal() {
            return None;
        }
        stty("-echo").then_some(NoEcho)
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        stty("echo");
    }
}

fn stty(arg: &str) -> bool {
    std::process::Command::new("stty")
        .arg(arg)
        .stdin(std::process::Stdio::inherit())
        .status()
        .is_ok_and(|s| s.success())
}

fn print_bundle(data: &serde_json::Value) {
    let Some(map) = data.as_object() else { return };
    for mime in TEXT_MIMES {
        if let Some(v) = map.get(*mime) {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            println!("{}", for_stdout(text.trim_end_matches('\n')));
            return;
        }
    }
    let keys: Vec<_> = map.keys().map(String::as_str).collect();
    eprintln!("[output: {}]", keys.join(", "));
}

fn for_stdout(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stdout().is_terminal() {
        s.into()
    } else {
        strip_ansi(s).into()
    }
}

fn for_stderr(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stderr().is_terminal() {
        s.into()
    } else {
        strip_ansi(s).into()
    }
}

/// Remove ANSI escape sequences (CSI and OSC), for output that is not a
/// terminal.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One JSON line per executed cell: status, count, and nbformat outputs.
pub fn json_line(index: usize, r: &ExecuteResult) -> serde_json::Value {
    let outputs: Vec<serde_json::Value> = r
        .outputs
        .iter()
        .map(|o: &Output| serde_json::to_value(o).unwrap_or_default())
        .collect();
    let mut v = serde_json::json!({
        "cell": index,
        "status": r.reply["status"],
        "execution_count": r.execution_count,
        "outputs": outputs,
    });
    if let Some((ename, evalue)) = r.error() {
        v["ename"] = ename.into();
        v["evalue"] = evalue.into();
    }
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_csi_and_osc() {
        assert_eq!(
            super::strip_ansi(
                "\u{1b}[31mError\u{1b}[0m: x \u{1b}]8;;http://a\u{7}link\u{1b}]8;;\u{7}"
            ),
            "Error: x link"
        );
    }
}
