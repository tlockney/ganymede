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

Five runs of each engine on CI runners (not a device); each cell is the
median of the five. `cold` is a new engine instance, loading the
workload and its first run; `steady` the median of four more runs in
that instance. `init` is V8's one-time setup per process.
`kernel-start` is today's in-app kernel (cdp-kernel 0.9.0's browser
kernel in a Worker) started to its first `execute_result`. One run
alone was misleading: the first draft of this note, from a single run,
had WebKit's first type strip slower than V8's. Five runs bear that out
on Linux (241 vs 214 ms) but not on the Mac (83 vs 138 ms).

#### macOS arm64 (GitHub runner, Apple silicon)

| workload | V8 15.2.124.1-rusty, jitless, 5 runs | V8 15.2.124.1-rusty, 5 runs | node, 5 runs | node, jitless, 5 runs | worker (webkit 26.6), 5 runs |
|---|---|---|---|---|---|
| init, once per process | 0.9 | 0.4 | – | – | – |
| start | 2.9 / 0.0 | 1.3 / 0.0 | 20 / 0.1 | 37 / 0.1 | 17 / 0.0 |
| cpu | 104 / 101 | 53 / 47 | 66 / 50 | 122 / 106 | 49 / 34 |
| ts-strip | 138 / 117 | 48 / 10 | 72 / 16 | 153 / 119 | 83 / 25 |
| large-output | 35 / 38 | 26 / 29 | 51 / 36 | 60 / 31 | 33 / 24 |
| memory | 122 / 128 | 37 / 43 | 56 / 42 | 147 / 144 | 79 / 53 |
| kernel-start | – | – | – | – | 41 / 0.5 |

Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):

| workload | V8 15.2.124.1-rusty, jitless, 5 runs | V8 15.2.124.1-rusty, 5 runs | node, 5 runs | node, jitless, 5 runs | worker (webkit 26.6), 5 runs |
|---|---|---|---|---|---|
| start | 1 | 1 | 9 | 7 | – |
| cpu | 62 | 75 | 53 | 52 | – |
| ts-strip | 49 | 49 | 56 | 47 | – |
| large-output | 134 | 85 | 227 | 102 | – |
| memory | 268 | 249 | 444 | 458 | – |
| kernel-start | – | – | – | – | – |
| peak RSS, whole run | 363 | 441 | 604 | 635 | – |

#### Linux x86_64 (GitHub runner)

| workload | V8 15.2.124.1-rusty, jitless, 5 runs | V8 15.2.124.1-rusty, 5 runs | node, 5 runs | node, jitless, 5 runs | worker (webkit 26.6), 5 runs |
|---|---|---|---|---|---|
| init, once per process | 0.6 | 0.6 | – | – | – |
| start | 2.4 / 0.0 | 2.6 / 0.0 | 34 / 0.1 | 65 / 0.1 | 19 / 0.0 |
| cpu | 114 / 107 | 75 / 68 | 97 / 77 | 142 / 114 | 59 / 39 |
| ts-strip | 214 / 185 | 103 / 22 | 136 / 30 | 240 / 182 | 241 / 37 |
| large-output | 62 / 75 | 47 / 50 | 96 / 67 | 111 / 48 | 47 / 35 |
| memory | 206 / 223 | 62 / 84 | 91 / 74 | 220 / 208 | 70 / 60 |
| kernel-start | – | – | – | – | 58 / 1.0 |

Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):

| workload | V8 15.2.124.1-rusty, jitless, 5 runs | V8 15.2.124.1-rusty, 5 runs | node, 5 runs | node, jitless, 5 runs | worker (webkit 26.6), 5 runs |
|---|---|---|---|---|---|
| start | 1 | 1 | 10 | 8 | – |
| cpu | 62 | 59 | 68 | 57 | – |
| ts-strip | 49 | 49 | 48 | 47 | – |
| large-output | 135 | 85 | 227 | 103 | – |
| memory | 279 | 249 | 463 | 459 | – |
| kernel-start | – | – | – | – | – |
| peak RSS, whole run | 357 | 424 | 612 | 618 | – |

What the numbers say, pending a device (Mac medians):

- **Start.** A jitless V8 isolate and context reach a first result in
  about 3 ms, plus about 1 ms of one-time init (the first start in a
  process ran 2 to 12 ms). A WebKit Worker took 17 ms (7 to 115 ms
  across runs), and today's kernel 41 ms to its first result. A V8
  kernel would add its own code on top: the browser kernel's bundle is
  about 840 KB, and loading the 700 KB stripper bundle took jitless V8
  about 13 ms.
- **Compute** is where the JIT shows, and WebKit in the apps keeps its
  JIT on iOS. Jitless V8 is about 1.5 to 5x slower than WebKit: the CPU
  cell 104 vs 49 ms on a first run (101 vs 34 steady), stripping types
  138 vs 83 ms first (117 vs 25 steady), the allocation-heavy cell 122
  vs 79 ms. Repeated type stripping is the worst case, about 4.7x
  slower once WebKit's JIT has warmed up. Node with `--jitless` lands
  with jitless V8, as it should.
- **Memory.** Jitless V8 kept the smallest process: about 360 MB peak
  for the whole run, against about 440 MB for V8 with its JIT and about
  610 MB for Node. The `memory` workload's 500,000 objects take about
  270 MB of heap in every V8; rusty_v8 builds without pointer
  compression, which would roughly halve that.

So: about 7 MB to download, a faster start, less memory, and compute
1.5 to 5x slower than WebKit (5x for repeated type stripping). Whether
that is acceptable is the call this spike was for. A typical cell (well
under 100 ms of work) would stay under a quarter of a second; a heavy
loop would take one and a half to five times as long as it does in
today's kernel. The device run will
show whether the ratios hold on an iPad and iPhone, and what iOS's
memory limit leaves. That run is by hand (the README's steps; listed
in ganymede-ui#273).
