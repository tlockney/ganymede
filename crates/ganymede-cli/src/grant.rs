//! `ganymede grant`: a notebook's own host API grant (docs/host-api.md),
//! kept by file id beside the server's file id database, so it follows a
//! rename made through the server. A key the notebook sets replaces the
//! `[host]` default for it; one it doesn't set keeps the default.

use anyhow::{bail, Context, Result};
use clap::Args;
use ganymede_server::host::Grant;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct GrantArgs {
    /// The notebook, a file under the root: relative to the current
    /// directory, or else to `--root-dir`.
    notebook: Option<PathBuf>,
    /// What to grant: `read=PATH` adds an API path the notebook may read
    /// and `run=PATH` one it may run (its own list replaces the `[host]`
    /// default; `""` is the root), and `edit_self=true|false` lets it
    /// edit its own cells or not. With `--revoke`, `read=PATH` or
    /// `run=PATH` removes a path, and a bare `read`, `run` or `edit_self`
    /// goes back to the default.
    grants: Vec<String>,
    /// Show the notebook's grant, or every notebook's with no NOTEBOOK.
    #[arg(long, conflicts_with_all = ["revoke", "grants"])]
    list: bool,
    /// Remove what follows instead; with nothing after NOTEBOOK, its
    /// whole grant.
    #[arg(long)]
    revoke: bool,
    /// The server's root directory, which API paths are relative to.
    #[arg(long, value_name = "DIR", default_value = ".")]
    root_dir: PathBuf,
}

/// One `grant` line: the notebook's path and its keys, as `ganymede
/// grant` takes them.
fn show(path: &str, grant: &Grant) -> String {
    let mut parts = vec![path.to_string()];
    if let Some(read) = &grant.read {
        if read.is_empty() {
            parts.push("read=(only itself)".into());
        }
        parts.extend(read.iter().map(|p| format!("read={p:?}")));
    }
    if let Some(run) = &grant.run {
        if run.is_empty() {
            parts.push("run=(none)".into());
        }
        parts.extend(run.iter().map(|p| format!("run={p:?}")));
    }
    if let Some(e) = grant.edit_self {
        parts.push(format!("edit_self={e}"));
    }
    parts.join(" ")
}

/// `notebook`'s API path under `root` (canonical). A relative path is
/// found from the current directory, or else from `root`. Only its
/// directory is resolved, so a symlinked notebook keeps its own name, the
/// path a kernel's session has.
fn api_path(root: &Path, notebook: &Path) -> Result<String> {
    let found = if notebook.exists() || notebook.is_absolute() {
        notebook.to_path_buf()
    } else {
        root.join(notebook)
    };
    if !found.is_file() {
        bail!("no notebook at {}", notebook.display());
    }
    let name = found
        .file_name()
        .with_context(|| format!("notebook {}", notebook.display()))?;
    let dir = match found.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let real = dir
        .canonicalize()
        .with_context(|| format!("notebook {}", notebook.display()))?
        .join(name);
    let rel = real
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("{} is not under {}", notebook.display(), root.display()))?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Ok(parts.context("the path isn't UTF-8")?.join("/"))
}

pub fn run(args: GrantArgs) -> Result<ExitCode> {
    let root = args
        .root_dir
        .canonicalize()
        .with_context(|| format!("root dir {}", args.root_dir.display()))?;
    let ids = ganymede_server::serve::file_ids(&root);
    if !ids.is_persistent() {
        bail!(
            "can't open the file id database in the Jupyter data dir, so a grant wouldn't be kept"
        );
    }
    let parse = |text: &str| -> Grant {
        serde_json::from_str(text).unwrap_or_else(|e| {
            eprintln!("warning: unreadable grant {text}: {e}");
            Grant::default()
        })
    };
    let Some(notebook) = &args.notebook else {
        if !args.list {
            bail!("name a notebook, or pass --list");
        }
        for (path, text) in ids.grants() {
            println!("{}", show(&path, &parse(&text)));
        }
        return Ok(ExitCode::SUCCESS);
    };
    let path = api_path(&root, notebook)?;
    if args.list {
        let grant = ids
            .get(&path)
            .and_then(|id| ids.grant(&id))
            .map(|t| parse(&t))
            .unwrap_or_default();
        if grant.is_empty() {
            println!("{path}: the [host] defaults");
        } else {
            println!("{}", show(&path, &grant));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if args.grants.is_empty() && !args.revoke {
        bail!("say what to grant, such as read=journal, run=reports or edit_self=false");
    }
    let (id, _) = ids.index(&path);
    let mut grant = ids.grant(&id).map(|t| parse(&t)).unwrap_or_default();
    if args.revoke && args.grants.is_empty() {
        grant = Grant::default();
    }
    for g in &args.grants {
        let (key, value) = match g.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (g.as_str(), None),
        };
        match (key, value, args.revoke) {
            (k @ ("read" | "run"), Some(p), false) => {
                let p = ganymede_contents::normalize_api_path(p);
                if p.split('/').any(|c| c == "..") {
                    bail!("{k}={p}: `..` is not allowed");
                }
                let list = if k == "read" {
                    &mut grant.read
                } else {
                    &mut grant.run
                };
                let list = list.get_or_insert_with(Vec::new);
                if !list.contains(&p) {
                    list.push(p);
                }
            }
            (k @ ("read" | "run"), Some(p), true) => {
                let p = ganymede_contents::normalize_api_path(p);
                let list = if k == "read" {
                    &mut grant.read
                } else {
                    &mut grant.run
                };
                if let Some(list) = list {
                    list.retain(|r| r != &p);
                }
            }
            ("read", None, true) => grant.read = None,
            ("run", None, true) => grant.run = None,
            ("edit_self", Some(v), false) => {
                grant.edit_self = Some(match v {
                    "true" => true,
                    "false" => false,
                    _ => bail!("edit_self takes true or false, not {v:?}"),
                });
            }
            ("edit_self", None, true) => grant.edit_self = None,
            _ => bail!(
                "can't {} {g:?}: use read=PATH, run=PATH or edit_self=true|false",
                if args.revoke { "revoke" } else { "grant" }
            ),
        }
    }
    let text = (!grant.is_empty()).then(|| serde_json::to_string(&grant).expect("a grant is JSON"));
    ids.set_grant(&id, text.as_deref())
        .map_err(|e| anyhow::anyhow!("saving the grant: {e}"))?;
    if grant.is_empty() {
        println!("{path}: the [host] defaults");
    } else {
        println!("{}", show(&path, &grant));
    }
    Ok(ExitCode::SUCCESS)
}
