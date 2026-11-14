//! `ganymede info`: versions, build details, and
//! every directory Ganymede reads or writes.

use anyhow::Result;
use ganymede_engine::host::kernelspec as ks;
use ganymede_engine::Engine;
use ganymede_server::lab::{data_dir, extensions_dir, find_extensions, installed, installs_dir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Everything `info` reports, as its `--json` object. Keys only get added.
pub fn collect(engine: &Engine, extra_roots: &[PathBuf]) -> Result<Value> {
    let specs: Vec<String> = engine.kernelspecs()?.into_iter().map(|s| s.name).collect();
    let lab_dir = installs_dir();
    let lab_versions: Vec<String> = lab_dir
        .as_deref()
        .map(installed)
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.version)
        .collect();
    let ext_dir = extensions_dir();
    let extensions: Vec<Value> = find_extensions(ext_dir.as_slice())
        .into_iter()
        .map(|e| json!({ "name": e.name, "version": e.version, "dir": e.dir }))
        .collect();
    let kernelspec_roots: Vec<PathBuf> = extra_roots
        .iter()
        .cloned()
        .chain(ks::jupyter_data_dirs())
        .collect();
    // The file read without --config, else where Ganymede looks first.
    let default = ganymede_server::serve::default_config();
    let config_file_warning = default.warning;
    let config_file = default.path.or_else(|| {
        ganymede_server::serve::ganymede_config_dir()
            .map(|d| d.join(ganymede_server::serve::DEFAULT_CONFIG_FILE))
    });
    let config_file_found = config_file.as_deref().is_some_and(Path::is_file);
    let tailnet = tailnet_identity(config_file.as_deref().filter(|_| config_file_found));
    let commit = env!("GANYMEDE_BUILD_COMMIT");
    Ok(json!({
        "version": env!("GANYMEDE_VERSION"),
        "protocol_version": ganymede_engine::protocol::PROTOCOL_VERSION,
        "build": {
            "commit": (!commit.is_empty()).then_some(commit),
            "target": env!("GANYMEDE_BUILD_TARGET"),
            "profile": env!("GANYMEDE_BUILD_PROFILE"),
        },
        "data_dirs": ks::jupyter_data_dirs(),
        "runtime_dir": ks::jupyter_runtime_dir(),
        "config_dir": ks::jupyter_config_dir(),
        "config_file": config_file,
        "config_file_found": config_file_found,
        "config_file_warning": config_file_warning,
        "data_dir": data_dir(),
        "lab_dir": lab_dir,
        "lab_versions": lab_versions,
        "labextensions_dir": ext_dir,
        "extensions": extensions,
        "kernelspec_roots": kernelspec_roots,
        "notary_dir": ks::jupyter_user_data_dir(),
        "console_history": crate::console::history_path(),
        "tailnet_identity": tailnet,
        "kernelspecs": specs,
    }))
}

/// `[tailscale]` from the config file: who `ganymede serve` lets in by
/// tailnet identity, whether it asks `tailscale whois`, and `error` when
/// `serve` would refuse to start with it. `null` when it lets no one in
/// that way, or the file doesn't read.
fn tailnet_identity(config_file: Option<&Path>) -> Value {
    let cfg = config_file
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| ganymede_server::config::Config::from_toml(&t).ok());
    match cfg {
        Some(c) if !c.tailscale.allow.is_empty() => json!({
            "allow": c.tailscale.allow,
            "whois": c.tailscale.whois,
            "error": ganymede_server::serve::check_tailscale(&c).err().map(|e| e.to_string()),
        }),
        _ => Value::Null,
    }
}

pub fn run(engine: &Engine, extra_roots: &[PathBuf], json: bool) -> Result<ExitCode> {
    let v = collect(engine, extra_roots)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", text(&v));
    }
    Ok(ExitCode::SUCCESS)
}

fn text(v: &Value) -> String {
    use std::fmt::Write;
    let s = |k: &str| v[k].as_str().unwrap_or("(none)").to_string();
    let list = |k: &str| -> Vec<String> {
        v[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = String::new();
    let _ = writeln!(out, "ganymede {}", s("version"));
    let b = &v["build"];
    let _ = writeln!(
        out,
        "build             {} {} {}",
        b["commit"].as_str().unwrap_or("(no commit)"),
        b["target"].as_str().unwrap_or_default(),
        b["profile"].as_str().unwrap_or_default()
    );
    let _ = writeln!(out, "protocol          {}", s("protocol_version"));
    let _ = writeln!(out, "runtime dir       {}", s("runtime_dir"));
    let found = v["config_file_found"].as_bool().unwrap_or(false);
    let _ = writeln!(
        out,
        "config file       {}{}",
        s("config_file"),
        if found { "" } else { " (not there)" }
    );
    if let Some(w) = v["config_file_warning"].as_str() {
        let _ = writeln!(out, "                  ({w})");
    }
    let _ = writeln!(out, "config dir        {}", s("config_dir"));
    let _ = writeln!(out, "data dir          {}", s("data_dir"));
    let versions = list("lab_versions");
    let _ = writeln!(
        out,
        "lab dir           {} ({})",
        s("lab_dir"),
        if versions.is_empty() {
            "none installed".to_string()
        } else {
            versions.join(", ")
        }
    );
    let _ = writeln!(out, "labextensions dir {}", s("labextensions_dir"));
    for e in v["extensions"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "  {} {}",
            e["name"].as_str().unwrap_or_default(),
            e["version"].as_str().unwrap_or_default()
        );
    }
    let _ = writeln!(out, "notary dir        {}", s("notary_dir"));
    let _ = writeln!(out, "console history   {}", s("console_history"));
    let t = &v["tailnet_identity"];
    let _ = match t["allow"].as_array() {
        Some(allow) => {
            let names: Vec<&str> = allow.iter().filter_map(Value::as_str).collect();
            let how = if t["whois"].as_bool().unwrap_or(false) {
                "tailscale serve or whois"
            } else {
                "tailscale serve"
            };
            match t["error"].as_str() {
                Some(e) => writeln!(
                    out,
                    "tailnet identity  {} (serve refuses to start: {e})",
                    names.join(", ")
                ),
                None => writeln!(out, "tailnet identity  {} (by {how})", names.join(", ")),
            }
        }
        None => writeln!(out, "tailnet identity  off"),
    };
    let _ = writeln!(out, "kernelspec roots:");
    for d in list("kernelspec_roots") {
        let _ = writeln!(out, "  {}", Path::new(&d).display());
    }
    let _ = writeln!(out, "data dirs:");
    for d in list("data_dirs") {
        let _ = writeln!(out, "  {d}");
    }
    let _ = writeln!(out, "kernelspecs: {}", list("kernelspecs").join(", "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_shows_every_path() {
        let v = json!({
            "version": "1.2.3",
            "protocol_version": "5.4",
            "build": { "commit": null, "target": "t", "profile": "debug" },
            "data_dirs": ["/d1"],
            "runtime_dir": "/run",
            "config_dir": "/cfg",
            "config_file": "/x/ganymede/ganymede.toml",
            "config_file_found": false,
            "data_dir": "/g",
            "lab_dir": "/g/lab",
            "lab_versions": ["4.6.4"],
            "labextensions_dir": "/g/labextensions",
            "extensions": [{ "name": "anywidget", "version": "0.9.0", "dir": "/g/labextensions/anywidget" }],
            "kernelspec_roots": ["/extra", "/d1"],
            "notary_dir": "/d1",
            "console_history": null,
            "tailnet_identity": { "allow": ["me@example.com", "tag:agent"], "whois": true },
            "kernelspecs": ["fake"],
        });
        let t = text(&v);
        for want in [
            "ganymede 1.2.3",
            "(no commit) t debug",
            "data dir          /g",
            "config file       /x/ganymede/ganymede.toml (not there)",
            "lab dir           /g/lab (4.6.4)",
            "labextensions dir /g/labextensions",
            "  anywidget 0.9.0",
            "notary dir        /d1",
            "console history   (none)",
            "tailnet identity  me@example.com, tag:agent (by tailscale serve or whois)",
            "kernelspec roots:\n  /extra\n  /d1",
        ] {
            assert!(t.contains(want), "{want:?} not in\n{t}");
        }
    }

    #[test]
    fn tailnet_identity_comes_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ganymede.toml");
        assert_eq!(tailnet_identity(None), Value::Null);
        std::fs::write(&file, "port = 8888\n").unwrap();
        assert_eq!(tailnet_identity(Some(&file)), Value::Null);
        std::fs::write(&file, "[tailscale]\nallow = [\"me@example.com\"]\n").unwrap();
        assert_eq!(
            tailnet_identity(Some(&file)),
            json!({ "allow": ["me@example.com"], "whois": false, "error": null })
        );
        std::fs::write(&file, "[tailscale]\nallow = [\"tag:agent\"]\n").unwrap();
        let t = tailnet_identity(Some(&file));
        assert!(t["error"].as_str().unwrap().contains("whois"), "{t}");
        let mut v = json!({});
        v["tailnet_identity"] = t;
        assert!(text(&v).contains("tailnet identity  tag:agent (serve refuses to start:"));
        let mut v = json!({});
        v["tailnet_identity"] = Value::Null;
        assert!(text(&v).contains("tailnet identity  off"));
    }
}
