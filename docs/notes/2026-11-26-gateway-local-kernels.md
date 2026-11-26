## 2026-11-26, gateway lists local kernels (#963)

With `gateway_url` set the server ran kernels on the remote Jupyter server
and listed only its kernelspecs, so a machine that could run kernels fine
here showed none of its own, and a gateway that was asleep left an empty
list with no reason given. Now this machine's kernels are listed beside
the gateway's, each spec says which side runs it, and `/api/kernelspecs`
says whether the gateway answered last time.

What changed:

- `crates/ganymede-server/src/combined.rs` (new): `Combined`, a
  `KernelspecSource`/`Provisioner`/`TransportFactory` over the gateway's
  triple and the local one, copying how `RegisteredKernels` routes. `list`
  is the gateway's specs first, then every local spec whose name
  (case-insensitive) the gateway lacks, and it sets each spec's `gateway`
  flag to the side that listed it. `launch` asks which side lists the name
  and launches there, keeping the side by the process handle's address in
  a map, as `RegisteredKernels` keeps its own; `connect` takes that entry
  and connects through that side, so a kernel comes back the way it went
  out. A name neither side lists is `NoSuchKernelspec`. The gateway wins a
  clash, so a notebook whose kernel the remote offers still runs there.
- `Kernelspec` (ganymede-kernel-host) has `gateway: bool`, `#[serde(skip)]`
  like `resource_dir`: set by the source that lists it, never read from
  `kernel.json`, and left out of a spec sent to a gateway or an app.
- `crates/ganymede-gateway/src/lib.rs`: `Status` (`reachable`, `error`,
  `at`) is what `refresh` found, and `Gateway::status` returns it.
  `refresh` now fetches through `Inner::fetch_specs` (which marks each spec
  as the gateway's) and records the outcome either way, so a failure keeps
  the kernelspecs it had. `Inner::scrub` takes the URL, its path and the
  token out of the error text, since the API reports it. `refresh_every`
  waits `every` while the gateway answers and `every.min(RETRY)` (15 s)
  while it doesn't, so a gateway that was asleep is found within 15 s.
  `Gateway::host` is the name to report: the URL's host, never the path,
  query or token.
- `crates/ganymede-server/src/serve.rs`: `Remote` is the gateway's triple
  plus an `Arc<dyn GatewayStatus>`; `local_kernels` builds the local triple
  from `Startup` where an app supplies it, else `default_provisioner`,
  `default_transports` and `FsKernelspecs`, and is `None` when this build
  can't run a kernel here (no `zmq`/`subprocess`, no app), where the
  gateway-only behavior stays. With a gateway and a local triple the
  source is `Combined`, and the status goes to the manager; without a
  gateway, nothing changes. `RemoteStatus` implements the API trait over
  the `Gateway` and the name (`gateway_name`, else the host).
- `Config.gateway_name` and `--gateway-name` name the gateway in the API.
- `crates/ganymede-api`: `GatewayModel { name, reachable, error }`;
  `KernelspecsModel.gateway: Option<GatewayModel>` and
  `KernelspecModel.gateway: bool` are extra keys, left out when unset or
  false, as `resource_dir` is (#891), so a local server's answer is as it
  was. `KernelManager::set_gateway_status` takes `Arc<dyn GatewayStatus>`,
  so the API crate doesn't depend on `ganymede-gateway`, and answers from
  it only once set.
- `Kernelspec { .. }` literals in tests (`sessions.rs`, `tests/host.rs`,
  `tests/http.rs`) take the new field.

Docs: `docs/kernels.md` "Kernels on another server", `docs/reference.md`
flags table, config sample and Gateway section, `docs/api.md` in "List
kernelspecs", each new section with the Unreleased note.

Tests:

- combined.rs: `lists_the_gateways_specs_then_this_machines_others` (order,
  case-insensitive clash, flags), `launch_and_connect_go_to_the_side_that_has_the_name`
  (each side launches its own, connect goes to that side, and a second
  connect for one launch fails), `a_name_neither_side_lists_is_no_such_kernelspec`.
- ganymede-gateway: `status_says_when_the_gateway_isnt_answering_and_when_it_is`
  (a stub that 503s, then answers: unreachable with the reason and no path
  or token in it, then reachable with one spec marked as the gateway's and
  the host as the name), `a_gateway_that_isnt_there_is_unreachable`.
- tests/gateway.rs: `lists_this_machines_kernels_beside_the_gateways_and_runs_each_there`
  (a real server and gateway: both lists, the flags and the top-level
  `gateway`, no token in the answer, a local spec starting here and a
  gateway spec starting there),
  `an_unreachable_gateway_still_lists_local_kernels_and_says_why` (closed
  port, `gateway_name`: local spec still listed, `reachable: false`, an
  error without the token or the URL's path).

Pre-push review round:

- A launch goes by the spec's own `gateway` flag, so a restart stays on
  the side the kernel started on even when the gateway's list changed
  since (a kernel started here before the gateway's first answer, or one
  whose name the gateway listed later); test
  `a_restart_stays_on_the_side_the_kernel_started_on`.
- The refresh interval is `next_refresh`, tested
  (`refreshes_sooner_while_the_gateway_isnt_answering`).
- `error` keeps the causes reqwest leaves out of its message (connection
  refused, DNS, timeout, TLS), and a remote's error body is cut to its
  first line, 200 characters (`a_remotes_error_body_is_cut_to_its_first_line`).
- `Remote` in serve.rs allows dead code without the `gateway` feature, for
  the `portability` job's `-D warnings` build.
- Docs: the state before the first refresh, the flag in the Unreleased
  note, and the `gateway` object being there whenever a gateway is set.
