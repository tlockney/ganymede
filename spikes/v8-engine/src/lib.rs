//! Spike (#646): runs the engine benchmark's workloads on V8 through
//! rusty_v8, to measure V8 for the in-app kernel (#266).
//!
//! Each workload runs in a fresh isolate, as a kernel start would: the
//! report times the isolate and context, loading the workload's scripts,
//! the first call of `workload.run()`, and further calls. The workloads
//! are plain scripts shared with the other engines (bench/workloads), so
//! the numbers compare across them. Nothing in Ganymede uses this crate.

use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};
use std::time::Instant;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// V8's `--jitless`. The iOS device build has no JIT whatever this says.
    #[serde(default = "yes")]
    pub jitless: bool,
    /// Calls of `workload.run()` per workload, the first included.
    #[serde(default = "five")]
    pub repeats: usize,
    pub workloads: Vec<Workload>,
}

fn yes() -> bool {
    true
}
fn five() -> usize {
    5
}

#[derive(Debug, Deserialize)]
pub struct Workload {
    pub name: String,
    /// Scripts run in order; the last defines `globalThis.workload`.
    pub sources: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub engine: String,
    pub version: String,
    pub jitless: bool,
    /// V8's platform and initialization, once per process.
    pub init_ms: f64,
    pub workloads: Vec<WorkloadReport>,
    /// The process's peak resident memory (getrusage), when known.
    pub max_rss_bytes: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub struct WorkloadReport {
    pub name: String,
    /// New isolate and context.
    pub start_ms: f64,
    /// Compiling and running the workload's scripts.
    pub load_ms: f64,
    /// The first `workload.run()`, including copying its result out.
    pub first_ms: f64,
    /// Median of the further calls, if any.
    pub steady_ms: Option<f64>,
    /// start + load + first: a cold start to the first result.
    pub cold_ms: f64,
    pub result_bytes: usize,
    pub result_preview: String,
    pub heap: Option<Heap>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Heap {
    pub used_bytes: usize,
    pub total_bytes: usize,
    pub physical_bytes: usize,
    pub external_bytes: usize,
    pub peak_malloced_bytes: usize,
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.total_cmp(b));
    let n = xs.len();
    Some(if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    })
}

/// The process's peak resident memory in bytes.
pub fn max_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage fills the struct it is given.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: rc == 0, so it was filled.
    let maxrss = unsafe { usage.assume_init() }.ru_maxrss as u64;
    // Bytes on Apple platforms, kilobytes elsewhere.
    Some(if cfg!(target_vendor = "apple") { maxrss } else { maxrss * 1024 })
}

#[cfg(feature = "v8")]
mod engine {
    use super::*;
    use std::sync::OnceLock;

    /// The jitless setting V8 was initialized with, and how long that took.
    static INIT: OnceLock<(bool, f64)> = OnceLock::new();

    pub fn init(jitless: bool) -> (bool, f64) {
        *INIT.get_or_init(|| {
            let t = Instant::now();
            if jitless {
                v8::V8::set_flags_from_string("--jitless");
            }
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
            (jitless, ms(t))
        })
    }

    pub fn run(config: &Config) -> Report {
        let (jitless, init_ms) = init(config.jitless);
        let workloads = config
            .workloads
            .iter()
            .map(|w| run_workload(w, config.repeats.max(1)))
            .collect();
        Report {
            engine: "v8".into(),
            version: v8::V8::get_version().into(),
            jitless,
            init_ms,
            workloads,
            max_rss_bytes: max_rss_bytes(),
        }
    }

    fn run_workload(w: &Workload, repeats: usize) -> WorkloadReport {
        let mut report = WorkloadReport {
            name: w.name.clone(),
            ..Default::default()
        };
        let t = Instant::now();
        let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
        {
            v8::scope!(let handle_scope, isolate);
            let context = v8::Context::new(handle_scope, Default::default());
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            report.start_ms = ms(t);

            v8::tc_scope!(let tc, scope);
            let t = Instant::now();
            for (i, src) in w.sources.iter().enumerate() {
                let code = v8::String::new(tc, src).expect("source fits a V8 string");
                let ok = v8::Script::compile(tc, code, None).and_then(|s| s.run(tc));
                if ok.is_none() {
                    let what = tc
                        .exception()
                        .map(|e| e.to_rust_string_lossy(tc))
                        .unwrap_or_else(|| "unknown error".into());
                    report.error = Some(format!("source {i}: {what}"));
                    return report;
                }
            }
            report.load_ms = ms(t);

            let call = v8::String::new(tc, "globalThis.workload.run()").unwrap();
            let script = v8::Script::compile(tc, call, None).unwrap();
            let mut times = Vec::with_capacity(repeats);
            for _ in 0..repeats {
                let t = Instant::now();
                let Some(value) = script.run(tc) else {
                    let what = tc
                        .exception()
                        .map(|e| e.to_rust_string_lossy(tc))
                        .unwrap_or_else(|| "unknown error".into());
                    report.error = Some(format!("run: {what}"));
                    return report;
                };
                // Copying the result out is part of the cost a kernel pays
                // to send an output.
                let out = value.to_rust_string_lossy(tc);
                times.push(ms(t));
                report.result_bytes = out.len();
                report.result_preview = out.chars().take(80).collect();
            }
            report.first_ms = times[0];
            report.steady_ms = median(times[1..].to_vec());
            report.cold_ms = report.start_ms + report.load_ms + report.first_ms;
        }
        let s = isolate.get_heap_statistics();
        report.heap = Some(Heap {
            used_bytes: s.used_heap_size(),
            total_bytes: s.total_heap_size(),
            physical_bytes: s.total_physical_size(),
            external_bytes: s.external_memory(),
            peak_malloced_bytes: s.peak_malloced_memory(),
        });
        report
    }
}

/// Runs the workloads. Without the `v8` feature (the size baseline) it
/// reports each workload as not run.
pub fn run(config: &Config) -> Report {
    #[cfg(feature = "v8")]
    {
        engine::run(config)
    }
    #[cfg(not(feature = "v8"))]
    {
        Report {
            engine: "none".into(),
            jitless: config.jitless,
            workloads: config
                .workloads
                .iter()
                .map(|w| WorkloadReport {
                    name: w.name.clone(),
                    error: Some("built without V8".into()),
                    ..Default::default()
                })
                .collect(),
            max_rss_bytes: max_rss_bytes(),
            ..Default::default()
        }
    }
}

fn run_json(config_json: &str) -> String {
    let report = match serde_json::from_str::<Config>(config_json) {
        Ok(config) => serde_json::to_value(run(&config)).unwrap(),
        Err(e) => serde_json::json!({ "error": format!("bad config: {e}") }),
    };
    report.to_string()
}

/// # Safety
/// `config_json` is a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn v8_spike_run(config_json: *const c_char) -> *mut c_char {
    let config = if config_json.is_null() {
        ""
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(config_json) }.to_str().unwrap_or("")
    };
    CString::new(run_json(config))
        .unwrap_or_default()
        .into_raw()
}

#[no_mangle]
pub extern "C" fn v8_spike_has_v8() -> i32 {
    cfg!(feature = "v8") as i32
}

/// # Safety
/// `s` came from `v8_spike_run`, or is null.
#[no_mangle]
pub unsafe extern "C" fn v8_spike_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: it came from CString::into_raw.
        drop(unsafe { CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[cfg(feature = "v8")]
    #[test]
    fn runs_a_workload_and_reports_errors() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "repeats": 3,
            "workloads": [
                { "name": "ok", "sources": ["globalThis.workload = { run: () => String(1 + 1) }"] },
                { "name": "throws", "sources": ["throw new Error('nope')"] },
            ],
        }))
        .unwrap();
        let report = run(&config);
        assert!(report.jitless);
        assert_eq!(report.workloads[0].result_preview, "2");
        assert!(report.workloads[0].steady_ms.is_some());
        assert!(report.workloads[0].heap.is_some());
        let err = report.workloads[1].error.as_deref().unwrap();
        assert!(err.contains("nope"), "{err}");
    }
}
