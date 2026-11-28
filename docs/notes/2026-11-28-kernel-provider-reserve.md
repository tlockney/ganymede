## 2026-11-28, kernel provider name reservation (#986)

`[kernel_providers] reserve` in `docs/kernel-provider.md`: kernelspec
name → a macOS code requirement, so a process that connects to the
providers' socket before the app can't take the app's name. The map is
`BTreeMap<String, String>`, empty by default, its keys lowercased on
load with a custom `deserialize_with`, since `register` lowercases the
names it is given.

Decisions worth the record:

- **The peer's audit token, not its pid.** On accept, next to
  `peer_cred`, the server reads the connecting process's audit token from
  the socket with `getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN, …)` into a
  32-byte buffer. A pid can be reused between the connection and the
  check, so it names nothing stable; the audit token is what
  `SecCodeCopyGuestWithAttributes` resolves to a code object. A failed
  call is one `warn!` and `None`, and a `None` token fails any
  reservation: there is no fallback to the pid.
- **One `unsafe` block, and why it is sound.** The whole reservation has
  one `unsafe`, inside `peer_audit_token`, `#[cfg(target_os = "macos")]`:
  `libc::getsockopt` writes into a `[u8; 32]` on the stack and the
  `socklen_t` length beside it, both owned by the function for the call,
  and the descriptor is the one the stream this connection owns was
  accepted on, alive for the duration. Nothing else in the path is
  `unsafe`: the Security check goes through `security-framework`.
- **The check is a pure-ish function.** `reservation_allows(&reserve,
  name, token: Option<&[u8; 32]>)` holds the rule and the platform split,
  so every case below is a unit test: a name not in `reserve` is allowed;
  a reserved name with no token is refused; a reserved name whose peer
  fails the requirement is refused. Inside, `peer_meets` parses the
  requirement with `SecRequirement::from_str`, builds `GuestAttributes`
  with `set_audit_token(CFData::from_buffer(token))`, takes the guest with
  `SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE)` and
  checks it with `check_validity(Flags::NONE, &requirement)`. Any error is
  the refusal, so the check fails closed. Off macOS `peer_meets` is an
  `Err` and `peer_audit_token` is `None`, so a reserved name is refused to
  everyone.
- **The refusal reads the same in every case.** `registered` with
  `error` `kernelspec <name> is reserved for another program`; the
  Security error and the reason for a missing token go to `debug!`, so a
  provider learns nothing about the requirement from the answer. A bad
  requirement in the config is also logged once at startup, when the
  listener starts, not only at the first refused `register`.
- **The reservation comes before the first-come check**, and before
  `RegisteredKernels::register`, so a reserved name is not the app's to
  give up: it is refused whether or not it is free.
- **Dependencies: `libc`, `security-framework` and `core-foundation`,
  macOS only.** `libc = "0.2"` and `security-framework = { version = "3",
  default-features = false, features = ["OSX_10_15"] }` are under
  `[target.'cfg(target_os = "macos")'.dependencies]`, as the brief has
  them. `core-foundation = "0.10"` is there too, which the brief did not
  list: `GuestAttributes::set_audit_token` takes a `CFDataRef`, and
  `security-framework` re-exports neither `CFData` nor
  `core-foundation`, so the token needs `CFData::from_buffer` from that
  crate directly. All three are already in `Cargo.lock` (through
  `rustls-platform-verifier` and `security-framework`); `Cargo.lock` gains
  no `[[package]]`, only the three names in `ganymede-server`'s dependency
  list. The features `security-framework` is asked for are the union
  Cargo already resolved, so the build compiles the same crate it did.

Tests:

- All platforms, unit: a name not reserved is allowed with `token: None`;
  a reserved name with `None` is refused; and, in the same test as the
  first two, a name with no reservation is still first come.
- `#[cfg(not(target_os = "macos"))]`, unit: a reserved name is refused
  even given `Some(token)`.
- `#[cfg(target_os = "macos")]`, unit: this process's own audit token,
  read from a `std::os::unix::net::UnixStream::pair()` with the same
  `peer_audit_token` the server uses (which also covers that function),
  meets `always`, and fails `never`, `identifier "no.such.app" and anchor
  apple generic`, and an unparsable `not a requirement((`.
- All platforms, integration (`tests/kernel_providers.rs`):
  `reserve = { "cdp-browser" = "never" }` into `kernel_providers::start`;
  a provider registering `cdp-browser` gets `registered` with an `error`
  containing "reserved", `/api/kernelspecs` doesn't list it, and
  `cdp-node` on the same connection registers, so the refusal is the
  reservation's and not the connection's.
- macOS only, integration: with `"always"`, the same provider registers
  the reserved name, and it is listed.

Removing the `reservation_allows` call in `handle` (passing an empty
map instead) fails `a_reserved_name_is_refused_and_another_registers`
with a bare `registered` for `cdp-browser`; the call is restored after
checking, and the whole suite passes.
