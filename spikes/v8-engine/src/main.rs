//! `v8-bench`: runs bench/workloads/manifest.json's workloads on V8 and
//! prints the report as JSON.
//!
//!   v8-bench [--jit] [--repeats N] [BENCH_DIR]
//!
//! BENCH_DIR (default `bench`) holds manifest.json's paths; build it first
//! with `npm run build` there. Jitless unless `--jit`.

use ganymede_v8_spike::{run, Config, Workload};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Manifest {
    workloads: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    sources: Vec<String>,
}

fn main() {
    let mut jitless = true;
    let mut repeats = 5;
    let mut dir = PathBuf::from("bench");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--jit" => jitless = false,
            "--repeats" => {
                repeats = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("--repeats takes a number")
            }
            "-h" | "--help" => {
                eprintln!("usage: v8-bench [--jit] [--repeats N] [BENCH_DIR]");
                return;
            }
            _ => dir = PathBuf::from(arg),
        }
    }
    let read = |rel: &str| {
        let path = dir.join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run `npm run build` in bench?)", path.display()))
    };
    let manifest: Manifest =
        serde_json::from_str(&read("workloads/manifest.json")).expect("manifest.json");
    let config = Config {
        jitless,
        repeats,
        workloads: manifest
            .workloads
            .into_iter()
            .map(|e| Workload {
                sources: e.sources.iter().map(|s| read(s)).collect(),
                name: e.name,
            })
            .collect(),
    };
    let report = run(&config);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
