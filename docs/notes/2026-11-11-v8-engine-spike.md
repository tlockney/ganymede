## 2026-11-11, measuring jitless V8 for the in-app kernel (#646)

Thomas chose to test V8 first for the in-app kernel's engine (#266) and
keep it unless its size or speed is unacceptable. `spikes/v8-engine`
measures it: its own Cargo workspace and workflow (`v8-spike.yml`),
which nothing in Ganymede depends on. The README has the steps,
including a bench app for a real iPad or iPhone.

What the build taught us:

- rusty_v8 publishes a device library for `aarch64-apple-ios`, built
  jitless and without WebAssembly by its CI, and the `v8` crate's build
  downloads it like any other target. No V8 build is needed.
- The crate's published source can't build V8 itself
  (`V8_FROM_SOURCE=1`): Cargo's packaging drops directories with their
  own `Cargo.toml` (the vendored crates under `third_party/rust`) and
  ICU's iOS data. A source build needs a rusty_v8 git checkout with its
  submodules, as rusty_v8's own CI does.

### Size

V8 152.2.0 (V8 15.2) linked into a stand-in iOS app, dead-stripped and
stripped, against the same app without it (`scripts/ios-size.sh`):

| | size |
|---|---|
| added to the app binary | 18.7 MB |
| the same, gzip -9 (about what a download adds) | 6.9 MB |
| static library before linking | 67 MB |

Not broken down further; ICU (for `Intl`) is likely a good part of it.
The design note for iPadOS guessed 30 to 50 MB.

### Speed and memory

One run each on CI runners (not a device), so treat a difference under
about 30% as noise. `cold` is a new engine instance, loading the
workload and its first run; `steady` the median of four more runs.
`kernel-start` is today's in-app kernel (cdp-kernel 0.9.0's browser
kernel in a Worker) started to its first `execute_result`.

#### macOS arm64 (GitHub runner, Apple silicon)

| workload | V8 15.2.124.1-rusty, jitless | V8 15.2.124.1-rusty | node | node, jitless | worker (webkit 26.6) |
|---|---|---|---|---|---|
| start | 12 / 0.0 | 18 / 0.0 | 43 / 0.1 | 60 / 0.1 | 93 / 0.5 |
| cpu | 112 / 99 | 83 / 52 | 91 / 57 | 151 / 135 | 134 / 52 |
| ts-strip | 147 / 127 | 69 / 15 | 100 / 22 | 209 / 136 | 349 / 47 |
| large-output | 44 / 51 | 36 / 48 | 71 / 43 | 75 / 59 | 92 / 44 |
| memory | 173 / 181 | 69 / 50 | 75 / 49 | 193 / 164 | 182 / 88 |
| kernel-start | – | – | – | – | 101 / 1.0 |

Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):

| workload | V8 15.2.124.1-rusty, jitless | V8 15.2.124.1-rusty | node | node, jitless | worker (webkit 26.6) |
|---|---|---|---|---|---|
| start | 1 | 1 | 9 | 7 | – |
| cpu | 62 | 60 | 61 | 41 | – |
| ts-strip | 49 | 49 | 56 | 41 | – |
| large-output | 134 | 85 | 234 | 224 | – |
| memory | 272 | 250 | 444 | 447 | – |
| kernel-start | – | – | – | – | – |
| peak RSS, whole run | 353 | 464 | 627 | 623 | – |

#### Linux x86_64 (GitHub runner)

| workload | V8 15.2.124.1-rusty, jitless | V8 15.2.124.1-rusty | node | node, jitless | worker (webkit 26.6) |
|---|---|---|---|---|---|
| start | 1.8 / 0.0 | 1.9 / 0.0 | 32 / 0.0 | 40 / 0.0 | 12 / 0.0 |
| cpu | 67 / 63 | 46 / 47 | 62 / 50 | 86 / 65 | 36 / 27 |
| ts-strip | 113 / 94 | 57 / 11 | 75 / 18 | 130 / 98 | 152 / 20 |
| large-output | 33 / 43 | 26 / 29 | 57 / 42 | 68 / 28 | 28 / 24 |
| memory | 113 / 123 | 40 / 54 | 59 / 53 | 123 / 122 | 55 / 42 |
| kernel-start | – | – | – | – | 40 / 0.5 |

Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):

| workload | V8 15.2.124.1-rusty, jitless | V8 15.2.124.1-rusty | node | node, jitless | worker (webkit 26.6) |
|---|---|---|---|---|---|
| start | 1 | 1 | 10 | 8 | – |
| cpu | 62 | 76 | 67 | 55 | – |
| ts-strip | 49 | 49 | 56 | 47 | – |
| large-output | 135 | 85 | 228 | 104 | – |
| memory | 266 | 250 | 443 | 460 | – |
| kernel-start | – | – | – | – | – |
| peak RSS, whole run | 358 | 429 | 598 | 615 | – |

What the numbers say, pending a device:

- **Start.** A jitless V8 isolate and context reach a first result in
  about 12 ms (the first isolate in the process; later ones about 1 ms).
  The first WebKit Worker in a page takes about 90 ms (later ones about
  10 ms), and today's kernel about 100 ms to its first result. A V8
  kernel would add its own code on top: the browser kernel's bundle is
  about 840 KB, and loading the 700 KB stripper bundle took jitless V8
  13 ms.
- **Steady work** is where the JIT shows. Jitless V8 is about 1.5 to 2x
  slower than WebKit's JIT on the CPU cell (99 vs 52 ms), 2.7x on
  stripping types again (127 vs 47 ms), and 2x on allocation-heavy work
  (181 vs 88 ms). Cold, it reverses: the first strip took jitless V8
  134 ms and WebKit 307 ms, before WebKit's JIT warmed up, and a cell
  usually runs once.
- **Memory.** Jitless V8 kept the smallest process: 353 MB peak for the
  whole run on the Mac, against 464 MB for V8 with its JIT and about
  620 MB for Node. The `memory` workload's 500,000 objects take about
  270 MB of heap in every V8; rusty_v8 builds without pointer
  compression, which would roughly halve that.

So far neither size nor speed looks unacceptable: about 7 MB to
download, a faster start, and steady compute at roughly half a JIT's
speed. The device run decides it, because iOS's WebKit keeps its JIT
while V8 can't, and the device's memory limit is real. That run is by
hand (the README's steps; listed in ganymede-ui#273).
