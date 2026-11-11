# V8 engine spike

A spike for [#646](https://github.com/tlockney/ganymede/issues/646): is
V8 without JIT (through [rusty_v8](https://github.com/denoland/rusty_v8))
small and fast enough to be the in-app kernel's engine on iPad and
iPhone ([#266](https://github.com/tlockney/ganymede/issues/266))? Nothing
here ships, and nothing in Ganymede depends on it: it is its own Cargo
workspace and its own workflow (`.github/workflows/v8-spike.yml`).

It measures two things:

- **Size.** `scripts/ios-size.sh` takes rusty_v8's library for
  `aarch64-apple-ios` (built jitless and without WebAssembly by rusty_v8's
  CI, as its build.rs sets for the device), links a stand-in app with and
  without it, and reports the difference, stripped and compressed.
- **Performance.** The same workloads on each engine, each workload in a
  fresh engine instance, as a kernel start would be:

  | workload | what it does |
  |---|---|
  | `start` | the smallest cell, `1 + 1`: cold start to the first result |
  | `cpu` | recursion, sorting 100,000 numbers, Map and string work, JSON, regex |
  | `ts-strip` | strips types from a 1,300-line TSX cell as the browser kernel does (sucrase, then acorn) |
  | `large-output` | builds a 3.3 MB CSV string and hands it to the host |
  | `memory` | keeps half a million objects and a 16 MB typed array alive |
  | `kernel-start` | (Worker page only) today's in-app kernel, cdp-kernel's browser kernel, started to its first `execute_result` |

  For each: `cold` (new engine, load the scripts, first run) and `steady`
  (median of four more runs) in ms, the engine's heap afterwards, and the
  process's peak memory.

The engines:

| runner | engine |
|---|---|
| `v8-bench` (`src/`) | V8 through rusty_v8, `--jitless` unless `--jit` |
| `bench/node.mjs` | Node, in a fresh `worker_threads` Worker per workload; `node --jitless` for its interpreter |
| `bench/web.mjs` | a browser's Workers through Playwright (WebKit, today's in-app engine), plus `kernel-start` |
| `ios/` | the bench app: jitless V8 linked in, and the Worker page in a WKWebView, on a real device |

## Run it on a computer

```sh
cd spikes/v8-engine
(cd bench && npm ci && npm run build && npx playwright install webkit)
mkdir -p results
cargo run --release --bin v8-bench -- bench > results/v8-jitless.json
cargo run --release --bin v8-bench -- --jit bench > results/v8-jit.json
node bench/node.mjs > results/node.json
node bench/web.mjs --browser webkit > results/webkit.json
node bench/report.mjs results/*.json
```

`v8-bench` uses rusty_v8's prebuilt library for the host. CI runs the same
on Linux and on an Apple silicon Mac, and puts the tables in the job
summary.

## Run it on an iPad or iPhone

By hand, on a device of your own. You need a Mac with Xcode, and a
signing team (a free Apple ID works for your own device). The app's
bundle ID, `com.ganymede-server.spike.V8Bench`, is not the Ganymede app's.

1. Build the device library (it downloads rusty_v8's, about a minute):

   ```sh
   rustup target add aarch64-apple-ios
   cd spikes/v8-engine
   scripts/ios-size.sh
   ```

   Or download the `v8-spike-ios` artifact from the latest run of the
   **V8 spike** workflow and put its `libganymede_v8_spike.a` at
   `spikes/v8-engine/target/aarch64-apple-ios/release/`.

2. Stage the workloads and generate the project:

   ```sh
   cd spikes/v8-engine
   (cd bench && npm ci && npm run build)
   scripts/stage-ios.sh
   brew install xcodegen
   (cd ios && xcodegen generate && open V8Bench.xcodeproj)
   ```

3. In Xcode, pick your team under Signing & Capabilities for the
   V8Bench target, pick the device, and Run (Cmd-R). Choose the Release
   configuration (Product > Scheme > Edit Scheme > Run > Build
   Configuration) so the Swift side doesn't skew the numbers.
4. On the device, tap **Run** and keep the app in front until it says
   Done (under a minute). Run it three times; the first run after
   install is the coldest.
5. Tap **Share results** and send the JSON to your Mac (AirDrop, or
   Save to Files). Then:

   ```sh
   node bench/report.mjs ~/Downloads/results*.json
   ```

   One file holds two reports, V8 and the Worker page. V8's carries the
   app's memory footprint (`footprint_peak_bytes`, what iOS counts
   against the app's limit) and the device model. The Worker's memory is
   the WebContent process's, which the app can't read, so it shows `–`.

## Caveats

- Jitless V8 on a Mac (CI's `--jitless` runs) uses a library built with
  the JIT and turned off at run time. The device library leaves the JIT
  out at build time. Both run V8's interpreter, so the Mac numbers are a guide to the
  device's, not a substitute.
- `start` for V8 is the isolate and context alone. A V8 kernel would add
  its kernel code (cdp-kernel's core, now run on the page) on top, so
  compare `kernel-start` to V8's `start` + the load of a kernel-sized
  script, roughly `ts-strip`'s `load`.
- V8 runs without pointer compression (rusty_v8's default); with it, the
  heap would be smaller.
