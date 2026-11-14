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
pub(crate) fn api_path(root: &Path, notebook: &Path) -> Result<String> {
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

/// A grant's `read=` or `run=` path as an API path; `..` is refused.
pub(crate) fn grant_path(key: &str, path: &str) -> Result<String> {
    let p = ganymede_contents::normalize_api_path(path);
    if p.split('/').any(|c| c == "..") {
        bail!("{key}={p}: `..` is not allowed");
    }
    Ok(p)
}

/// A grant held on a file, under one root.
pub(crate) struct Held {
    ids: ganymede_contents::FileIds,
    id: String,
    root: PathBuf,
    api: String,
    /// The stamp the grant was stored under when it was last checked, for
    /// [`restamp_held`]; `None` when it no longer stands.
    seen: Option<Option<String>>,
}

/// The grants held on the file at `path` under every root that has one:
/// a program that writes a notebook (`ganymede execute`, `nb clear`)
/// doesn't know which root, a server's or `ganymede grant`'s, its grant
/// was made under, and it may be run from a subdirectory, or from
/// elsewhere (#767).
fn held(path: &Path) -> Vec<Held> {
    let Some(db) = ganymede_server::serve::file_ids_db() else {
        return Vec::new();
    };
    // The directory resolved, the name as it is, as `api_path` does, so a
    // symlinked notebook keeps its own name.
    let Ok(abs) = std::path::absolute(path) else {
        return Vec::new();
    };
    let (Some(dir), Some(name)) = (abs.parent(), abs.file_name()) else {
        return Vec::new();
    };
    let Ok(dir) = dir.canonicalize() else {
        return Vec::new();
    };
    ganymede_contents::roots_with_id(&db, &dir.join(name))
        .into_iter()
        .filter_map(|(root, api)| {
            let ids = ganymede_contents::FileIds::open(&db, &root);
            let id = ids.get(&api)?;
            ids.grant(&id)?;
            Some(Held {
                ids,
                id,
                root,
                api,
                seen: None,
            })
        })
        .collect()
}

/// Before a program writes the notebook at `path`: drops the grants,
/// under every root, whose file changed outside Ganymede (#767). What
/// comes back is for [`restamp_held`] after the write.
pub(crate) fn check_held(path: &Path) -> Vec<Held> {
    let mut all = held(path);
    for h in &mut all {
        h.seen = h
            .ids
            .check_for_write_with(&h.id, || ganymede_contents::api_file_stamp(&h.root, &h.api));
    }
    all
}

/// The program wrote the notebook: the grants that stood before keep
/// matching it.
pub(crate) fn restamp_held(held: &[Held]) -> Result<()> {
    for h in held {
        h.ids
            .restamp_if(&h.id, &h.seen, |p| {
                ganymede_contents::api_file_stamp(&h.root, p)
            })
            .map_err(|e| anyhow::anyhow!("keeping the grant for {}: {e}", h.api))?;
    }
    Ok(())
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
        // Only grants whose files are as Ganymede left them: one whose
        // file changed outside it is dropped here (#767).
        for (path, text) in ids.grants_checked(|p| ganymede_contents::api_file_stamp(&root, p)) {
            println!("{}", show(&path, &parse(&text)));
        }
        return Ok(ExitCode::SUCCESS);
    };
    let path = api_path(&root, notebook)?;
    let stamp = || ganymede_contents::api_file_stamp(&root, &path);
    if args.list {
        let grant = ids
            .get(&path)
            .and_then(|id| ids.grant_checked_with(&id, stamp))
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
    // One its file outgrew is gone, so this starts from the defaults.
    let mut grant = ids
        .grant_checked_with(&id, stamp)
        .map(|t| parse(&t))
        .unwrap_or_default();
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
                let p = grant_path(k, p)?;
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
    ids.set_grant_with(&id, text.as_deref(), |p| {
        ganymede_contents::api_file_stamp(&root, p)
    })
    .map_err(|e| anyhow::anyhow!("saving the grant: {e}"))?;
    if grant.is_empty() {
        println!("{path}: the [host] defaults");
    } else {
        println!("{}", show(&path, &grant));
    }
    Ok(ExitCode::SUCCESS)
}
