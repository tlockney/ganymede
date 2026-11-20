//! The engine benchmark on V8 (#892), for the apps' Settings: runs the
//! workloads of `spikes/v8-engine/bench/workloads` through rusty_v8 and
//! returns a JSON report. It is the spike's runner (`spikes/v8-engine/src`,
//! #646) brought into the library the apps already link, since a second
//! Rust static library in an app would clash with this one's runtime
//! symbols. Keep the two in step: the config and the report are the same.
//!
//! Each workload runs in a fresh isolate, as a kernel start would: the
//! report times the isolate and context, loading the workload's scripts,
//! the first call of `workload.run()`, and further calls.
//!
//! The functions are always exported. Without the `v8-bench` feature,
//! [`ganymede_v8_bench_available`] is 0 and a run returns an error report.

use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Mutex;

/// The stack each run's thread gets. V8 derives its own stack limit from
/// the thread it runs on, so a caller's small thread (a Swift task's)
/// must not matter.
const STACK_BYTES: usize = 8 * 1024 * 1024;

/// V8's platform, flags and initialization are process-global: one run at
/// a time.
static RUN_LOCK: Mutex<()> = Mutex::new(());

/// 1 when the library was built with the `v8-bench` feature, else 0.
#[no_mangle]
pub extern "C" fn ganymede_v8_bench_available() -> c_int {
    cfg!(feature = "v8-bench") as c_int
}

/// Runs a benchmark and returns its report as JSON, for
/// [`ganymede_v8_bench_free`]. `config_json` is
/// `{"jitless": bool, "repeats": n, "workloads": [{"name": "...",
/// "sources": ["js", ...]}]}` (`jitless` defaults to true, `repeats` to
/// 5, at most 1000). Whatever goes wrong (NULL or invalid UTF-8 or
/// JSON, too many repeats, a panic, a library without V8) is a report `{"error": "..."}`; a workload whose
/// script throws carries its own `error`.
///
/// Blocks until the workloads are done, on a thread of its own with an
/// 8 MB stack, and one call at a time. V8 initializes on the first call,
/// which also sets its flags: later calls keep that call's `jitless`
/// (the report says which). The iOS device library is built without a JIT
/// and always reports `jitless: true`; the simulator's keeps it. A Mac app
/// under the hardened runtime without the allow-jit entitlement aborts on
/// `"jitless": false`, since V8 can't map executable memory.
///
/// A workload that never returns holds the call, and the lock for the
/// others, for good; V8 running out of heap aborts the process. Neither is
/// guarded: the workloads are the benchmark's own.
///
/// # Safety
///
/// `config_json` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn ganymede_v8_bench_run(config_json: *const c_char) -> *mut c_char {
    let config = if config_json.is_null() {
        Err("config is NULL".to_string())
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(config_json) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| "config is not valid UTF-8".to_string())
    };
    let report = match config {
        Ok(config) => run_on_thread(config),
        Err(e) => error_report(&e),
    };
    // The report is JSON, which escapes NULs.
    CString::new(report).unwrap_or_default().into_raw()
}

/// Frees a string [`ganymede_v8_bench_run`] returned. NULL is ignored.
///
/// # Safety
///
/// `s` came from [`ganymede_v8_bench_run`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn ganymede_v8_bench_free(s: *mut c_char) {
    // SAFETY: the same contract as ganymede_string_free.
    unsafe { crate::ganymede_string_free(s) }
}

fn error_report(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn run_on_thread(config: String) -> String {
    guarded(move || run_json(&config))
}

/// Runs `work` on a thread with [`STACK_BYTES`] of stack, one at a time,
/// and turns a panic into an error report. The crate's panic strategy is
/// unwind (the release profile doesn't set `abort`), so the catch works.
fn guarded(work: impl FnOnce() -> String + Send + 'static) -> String {
    // A panic in an earlier run poisons nothing that matters: the lock
    // guards no data.
    let _guard = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let spawned = std::thread::Builder::new()
        .name("ganymede-v8-bench".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|payload| {
                let what = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                error_report(&format!("benchmark panicked: {what}"))
            })
        });
    match spawned {
        Ok(handle) => handle
            .join()
            .unwrap_or_else(|_| error_report("benchmark thread panicked")),
        Err(e) => error_report(&format!("could not start the benchmark thread: {e}")),
    }
}

#[cfg(not(feature = "v8-bench"))]
fn run_json(_config: &str) -> String {
    error_report("built without V8 (ganymede-ffi's `v8-bench` feature)")
}

#[cfg(feature = "v8-bench")]
use engine::run_json;

#[cfg(feature = "v8-bench")]
mod engine {
    use serde::{Deserialize, Serialize};
    use std::sync::OnceLock;
    use std::time::Instant;

    #[derive(Debug, Deserialize)]
    struct Config {
        /// V8's `--jitless`, as the first call sets it.
        #[serde(default = "yes")]
        jitless: bool,
        /// Calls of `workload.run()` per workload, the first included.
        #[serde(default = "five")]
        repeats: usize,
        workloads: Vec<Workload>,
    }

    fn yes() -> bool {
        true
    }
    fn five() -> usize {
        5
    }

    #[derive(Debug, Deserialize)]
    struct Workload {
        name: String,
        /// Scripts run in order; the last defines `globalThis.workload`.
        sources: Vec<String>,
    }

    #[derive(Debug, Default, Serialize)]
    struct Report {
        engine: String,
        version: String,
        jitless: bool,
        /// V8's platform and initialization, once per process.
        init_ms: f64,
        workloads: Vec<WorkloadReport>,
        /// The process's peak resident memory (getrusage), when known.
        max_rss_bytes: Option<u64>,
    }

    #[derive(Debug, Default, Serialize)]
    struct WorkloadReport {
        name: String,
        /// New isolate and context.
        start_ms: f64,
        /// Compiling and running the workload's scripts.
        load_ms: f64,
        /// The first `workload.run()`, including copying its result out.
        first_ms: f64,
        /// Median of the further calls, if any.
        steady_ms: Option<f64>,
        /// start + load + first: a cold start to the first result.
        cold_ms: f64,
        result_bytes: usize,
        result_preview: String,
        heap: Option<Heap>,
        error: Option<String>,
    }

    #[derive(Debug, Default, Serialize)]
    struct Heap {
        used_bytes: usize,
        total_bytes: usize,
        physical_bytes: usize,
        external_bytes: usize,
        peak_malloced_bytes: usize,
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
    fn max_rss_bytes() -> Option<u64> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage fills the struct it is given.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: rc == 0, so it was filled.
        let maxrss = unsafe { usage.assume_init() }.ru_maxrss as u64;
        // Bytes on Apple platforms, kilobytes elsewhere.
        Some(if cfg!(target_vendor = "apple") {
            maxrss
        } else {
            maxrss * 1024
        })
    }

    /// The iOS device library is built without a JIT: it is jitless whatever
    /// the config says. (The simulator's library keeps the JIT.)
    const NO_JIT_BUILD: bool = cfg!(all(target_os = "ios", not(target_abi = "sim")));

    /// The jitless setting V8 was initialized with and how long that took,
    /// or why it failed. A failure stays: a second initialization after a
    /// panic part-way would abort.
    static INIT: OnceLock<Result<(bool, f64), String>> = OnceLock::new();

    fn init(jitless: bool) -> Result<(bool, f64), String> {
        init_once(&INIT, || {
            let jitless = jitless || NO_JIT_BUILD;
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

    /// Runs `f` once for `cell`; a panic in it is kept as an error, so later
    /// calls report it instead of running `f` again.
    fn init_once(
        cell: &OnceLock<Result<(bool, f64), String>>,
        f: impl FnOnce() -> (bool, f64),
    ) -> Result<(bool, f64), String> {
        cell.get_or_init(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .map_err(|_| "V8 failed to initialize earlier in this process".to_string())
        })
        .clone()
    }

    pub(super) fn run_json(config_json: &str) -> String {
        match serde_json::from_str::<Config>(config_json) {
            Ok(config) => match run(&config) {
                Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
                    super::error_report(&format!("could not write the report: {e}"))
                }),
                Err(e) => super::error_report(&e),
            },
            Err(e) => super::error_report(&format!("bad config: {e}")),
        }
    }

    /// The most `repeats` a config may ask for.
    const MAX_REPEATS: usize = 1000;

    fn run(config: &Config) -> Result<Report, String> {
        if config.repeats > MAX_REPEATS {
            return Err(format!(
                "repeats {} is over the limit of {MAX_REPEATS}",
                config.repeats
            ));
        }
        let (jitless, init_ms) = init(config.jitless)?;
        let workloads = config
            .workloads
            .iter()
            .map(|w| run_workload(w, config.repeats.max(1)))
            .collect();
        Ok(Report {
            engine: "v8".into(),
            version: v8::V8::get_version().into(),
            jitless,
            init_ms,
            workloads,
            max_rss_bytes: max_rss_bytes(),
        })
    }

    /// The try-catch's exception as text. A macro: the scope types are
    /// too nested to name in a function.
    macro_rules! exception_text {
        ($tc:expr) => {
            $tc.exception()
                .map(|e| e.to_rust_string_lossy($tc))
                .unwrap_or_else(|| "unknown error".into())
        };
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
                let Some(code) = v8::String::new(tc, src) else {
                    report.error = Some(format!("source {i}: too long for a V8 string"));
                    return report;
                };
                let ok = v8::Script::compile(tc, code, None).and_then(|s| s.run(tc));
                if ok.is_none() {
                    report.error = Some(format!("source {i}: {}", exception_text!(tc)));
                    return report;
                }
            }
            report.load_ms = ms(t);

            let call = v8::String::new(tc, "globalThis.workload.run()").unwrap();
            let Some(script) = v8::Script::compile(tc, call, None) else {
                report.error = Some(format!("run: {}", exception_text!(tc)));
                return report;
            };
            let mut times = Vec::with_capacity(repeats);
            for _ in 0..repeats {
                let t = Instant::now();
                let Some(value) = script.run(tc) else {
                    report.error = Some(format!("run: {}", exception_text!(tc)));
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

    #[cfg(test)]
    mod tests {
        use super::{init_once, median};
        use std::sync::OnceLock;

        #[test]
        fn a_failed_init_is_remembered() {
            let cell = OnceLock::new();
            let first = init_once(&cell, || panic!("init failed"));
            assert_eq!(
                first,
                Err("V8 failed to initialize earlier in this process".to_string())
            );
            let mut ran = false;
            let second = init_once(&cell, || {
                ran = true;
                (true, 0.0)
            });
            assert_eq!(second, first);
            assert!(!ran, "a second call must not initialize again");
        }

        #[test]
        fn median_odd_even_empty() {
            assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
            assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
            assert_eq!(median(vec![]), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// A call through the C ABI, as the apps make it.
    fn call(config: Option<&str>) -> Value {
        let c = config.map(|s| CString::new(s).unwrap());
        let ptr = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: ptr is NULL or a NUL-terminated string that outlives the call.
        let out = unsafe { ganymede_v8_bench_run(ptr) };
        assert!(!out.is_null());
        // SAFETY: out is a NUL-terminated string from ganymede_v8_bench_run.
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        // SAFETY: out came from ganymede_v8_bench_run and isn't used again.
        unsafe { ganymede_v8_bench_free(out) };
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
    }

    #[test]
    fn free_ignores_null() {
        // SAFETY: NULL is allowed.
        unsafe { ganymede_v8_bench_free(std::ptr::null_mut()) };
    }

    #[cfg(not(feature = "v8-bench"))]
    #[test]
    fn without_v8_it_says_so() {
        assert_eq!(ganymede_v8_bench_available(), 0);
        let report = call(Some(r#"{"workloads": []}"#));
        let error = report["error"].as_str().unwrap();
        assert!(error.contains("built without V8"), "{error}");
        assert!(call(None)["error"].as_str().unwrap().contains("NULL"));
    }

    #[cfg(feature = "v8-bench")]
    mod with_v8 {
        use super::*;
        use serde_json::json;

        /// The smallest workload, as spikes/v8-engine/bench/workloads/start.js
        /// has it (not included from there: spikes/ changes don't run this
        /// crate's CI).
        const START: &str = "globalThis.workload = { run: () => String(1 + 1) };";

        fn number(v: &Value) -> f64 {
            v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
        }

        #[test]
        fn runs_the_start_workload() {
            assert_eq!(ganymede_v8_bench_available(), 1);
            let config = json!({
                "repeats": 3,
                "workloads": [{ "name": "start", "sources": [START] }],
            });
            let report = call(Some(&config.to_string()));
            assert!(report.get("error").is_none(), "{report}");
            assert_eq!(report["engine"], "v8");
            assert!(!report["version"].as_str().unwrap().is_empty());
            assert!(number(&report["init_ms"]) >= 0.0);
            let w = &report["workloads"][0];
            assert_eq!(w["name"], "start");
            assert!(w["error"].is_null(), "{w}");
            assert_eq!(w["result_preview"], "2");
            assert_eq!(w["result_bytes"], 1);
            for key in ["start_ms", "load_ms", "first_ms", "steady_ms", "cold_ms"] {
                assert!(number(&w[key]) >= 0.0, "{key}: {w}");
            }
            assert!(w["heap"]["used_bytes"].as_u64().unwrap() > 0);
        }

        #[test]
        fn a_second_call_works_and_keeps_the_first_jitless() {
            let first = call(Some(
                &json!({ "jitless": true, "workloads": [{ "name": "a", "sources": [START] }] })
                    .to_string(),
            ));
            let second = call(Some(
                &json!({ "jitless": false, "workloads": [{ "name": "b", "sources": [START] }] })
                    .to_string(),
            ));
            // Whichever test in this process ran first set it.
            assert_eq!(first["jitless"], second["jitless"]);
            assert_eq!(first["init_ms"], second["init_ms"]);
            assert_eq!(second["workloads"][0]["result_preview"], "2");
        }

        #[test]
        fn a_throwing_workload_reports_its_error() {
            let config = json!({ "workloads": [
                { "name": "throws", "sources": ["throw new Error('nope')"] },
                { "name": "no-workload", "sources": ["1"] },
                { "name": "syntax", "sources": ["let ("] },
                { "name": "runs", "sources": [START] },
            ]});
            let report = call(Some(&config.to_string()));
            assert!(report.get("error").is_none(), "{report}");
            let ws = report["workloads"].as_array().unwrap();
            assert!(
                ws[0]["error"].as_str().unwrap().contains("nope"),
                "{}",
                ws[0]
            );
            assert!(
                ws[1]["error"].as_str().unwrap().starts_with("run:"),
                "{}",
                ws[1]
            );
            assert!(
                ws[2]["error"].as_str().unwrap().starts_with("source 0:"),
                "{}",
                ws[2]
            );
            // The workloads after a failed one still run.
            assert!(ws[3]["error"].is_null(), "{}", ws[3]);
            assert_eq!(ws[3]["result_preview"], "2");
        }

        #[test]
        fn too_many_repeats_is_an_error() {
            let config = json!({ "repeats": 1_000_000_000, "workloads": [] });
            let report = call(Some(&config.to_string()));
            assert!(
                report["error"].as_str().unwrap().contains("repeats"),
                "{report}"
            );
        }

        /// Recursion about 5,000 frames deep needs far more than a 64 KB
        /// stack, and fits V8's own limit. It runs on the library's thread,
        /// whatever the caller's stack is.
        #[test]
        fn deep_recursion_from_a_small_stack_works() {
            let deep = "function f(n) { return n === 0 ? 0 : 1 + f(n - 1); }
                globalThis.workload = { run: () => String(f(5000)) };";
            let caller = std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn(move || {
                    let config = json!({ "workloads": [{ "name": "deep", "sources": [deep] }] });
                    call(Some(&config.to_string()))
                })
                .unwrap();
            let report = caller.join().unwrap();
            assert_eq!(report["workloads"][0]["result_preview"], "5000", "{report}");
        }

        #[test]
        fn runs_from_threads_with_small_stacks_at_once() {
            let handles: Vec<_> = (0..3)
                .map(|_| {
                    std::thread::Builder::new()
                        .stack_size(64 * 1024)
                        .spawn(|| {
                            let config =
                                json!({ "workloads": [{ "name": "s", "sources": [START] }] });
                            call(Some(&config.to_string()))
                        })
                        .unwrap()
                })
                .collect();
            for h in handles {
                let report = h.join().unwrap();
                assert_eq!(report["workloads"][0]["result_preview"], "2", "{report}");
            }
        }
    }

    #[cfg(feature = "v8-bench")]
    #[test]
    fn bad_input_is_named() {
        assert!(call(None)["error"].as_str().unwrap().contains("NULL"));
        assert!(call(Some("not json"))["error"]
            .as_str()
            .unwrap()
            .starts_with("bad config"));
        // Missing "workloads".
        assert!(call(Some(r#"{"repeats": 1}"#))["error"]
            .as_str()
            .unwrap()
            .starts_with("bad config"));
        let bad = [0xffu8, 0];
        // SAFETY: a NUL-terminated string (not UTF-8).
        let out = unsafe { ganymede_v8_bench_run(bad.as_ptr().cast()) };
        // SAFETY: out is a NUL-terminated string from ganymede_v8_bench_run.
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        // SAFETY: out came from ganymede_v8_bench_run.
        unsafe { ganymede_v8_bench_free(out) };
        assert!(text.contains("UTF-8"), "{text}");
    }

    /// Without RUN_LOCK the two closures overlap.
    #[test]
    fn runs_are_one_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (in_flight, max, start) = (in_flight.clone(), max.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    guarded(move || {
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max.fetch_max(now, Ordering::SeqCst);
                        // Long enough for another run to start, if it could.
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        "{}".into()
                    })
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(max.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panic_is_an_error_report() {
        let report: Value = serde_json::from_str(&guarded(|| panic!("boom {}", 1))).unwrap();
        assert_eq!(report["error"], "benchmark panicked: boom 1");
        // The lock isn't left held, and a later run works.
        assert_eq!(guarded(|| "ok".into()), "ok");
    }
}
