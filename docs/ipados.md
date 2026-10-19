# The iPad use case

The [ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados) design puts an iPad client in front
of kernels on the homelab over the tailnet, with a local in-process kernel
later. Its review established that remote clients speak the server's REST
API and websocket, never ZMTP, and that iCloud Drive, not SMB, is the file
sync story. Read against that plan, Ganymede is the remote tier, and
the plan adds the following requirements.

Status: token auth, atomic writes with the mtime check, a configurable
checkpoint directory, the attach provisioner, idle culling, verbatim
metadata passthrough, and reattaching to a kernel after a disconnect
with the missed output replayed are built. The Mac and iPad app exists
([ganymede-ui architecture](ganymede-ui/architecture.md#mac-and-ipad-app)):
it runs the server in-process through `ganymede-ffi`, and on the iPad
kernels run in the web view or on a remote server through the gateway.
`tailscale serve` identity headers, reloading files changed on disk, and
a native in-process kernel are not built; see the [roadmap](roadmap.md).
The rest of this page is the plan as written before the app, kept for
its reasoning.

Tier 0, remote validation. The plan runs a Jupyter server plus cdp-kernel
on the Mac mini with Juno Connect or JupyterLab in Safari as the client.
Ganymede replaces the Python one there, which means:

- REST API fidelity. Juno Connect and JupyterLab are third-party clients
  of the API; anything nonstandard in `/api/contents`, `/api/sessions`, or
  the websocket subprotocols breaks them. The API is a compatibility
  target, not a design space, until a bespoke client exists.
- Token auth as the baseline. The review names a tokenless,
  allow-all-origins setup as the thing not to copy. An alternative worth
  having: when served through `tailscale serve`, trust the identity
  headers it injects and skip tokens. Both are small.
- Kernels survive client disconnects. The iPad suspends the app; the
  websocket drops; the kernel must keep running and the client must
  reattach by kernel id with iopub resumed. Idle culling with a
  configurable timeout is the complement, so abandoned kernels do not
  pile up.
- Files are the source of truth, not server memory. With the notebook
  root in iCloud Drive, the iPad writes files underneath the server. The
  contents API needs external-change detection, atomic writes (temp file
  plus rename, so iCloud never syncs a half-written notebook), and a
  checkpoint directory configurable to a path outside the synced folder. `jupyter_server` has
  no change check on PUT (JupyterLab compares timestamps itself before
  saving); Ganymede adds an opt-in one: a PUT whose body carries
  `last_modified` gets 409 if the file on disk is newer by more than
  500 ms. For an exact check, a content `GET` and every `PUT` carry the
  file's sha256 as `ETag`, and a `PUT` with `If-Match` gets 412 unless the
  file still has that hash. Clients that send neither, such as VS Code and
  JupyterLab, save unconditionally.
- Runs as a daemon: launchd on the Mac mini, one config file, logs to a
  file.

Tier 1, bespoke client. The client is HTTP plus websocket in Swift. That
makes the websocket protocol and the channel-message shape a public
contract of Ganymede. Document it as such.

Tier 2, local kernel. Two implications, both about packaging rather than
features:

- The review's point that the local kernel should speak the same
  channel-JSON shape the websocket carries, with no ZMTP, means the
  interesting boundary in this codebase is "a stream of channel messages
  to and from a kernel", with ZMQ as one transport behind it. Put that
  boundary in a library crate with no tokio-net or ZMQ dependency of its
  own, so the iPad app can compile the protocol, nbformat, and document
  crates for iOS (via uniffi or swift-bridge) and drive its in-process
  kernel through the same types. Workspace shape: `protocol`, `nbformat`,
  `document`, `kernel-host` (transport trait plus the ZMQ and in-process
  implementations), `server` (axum binary). The server is one consumer of
  the libraries; the app is another.
- The plan's development loop drives the device kernel from JupyterLab on
  the Mac through a ZMTP listener on the iPad. That needs an "attach to
  an existing kernel" provisioner: given a connection file or ip and
  ports, connect without launching. `jupyter console --existing` is the
  precedent. Cheap, and it is also how a kernel inside a container or on
  another host gets used.

Capability manifests. cdp-kernel already writes `metadata` into
`kernel.json` and JupyterLab reads `metadata.debugger` from it. The plan
adds `metadata.capabilities` and the same object in `kernel_info_reply`.
The server must pass kernelspec metadata and `kernel_info_reply` through
verbatim, with no schema that strips unknown fields. Worth a test.

A tension to decide early. The [document model](protocol.md#document-model-and-collaboration) could be a `yrs` CRDT.
With iCloud as the sync story, there are two writers of the same file:
the server and the Files app. A server-side CRDT is authoritative only
for clients connected to it; the file on disk must still be re-read when
it changes underneath. Either treat the file as truth and keep the CRDT
as a live session cache that reloads on external change, or drop the
CRDT for v1 and rely on last-writer-wins with the mtime check. For this
use case, the second is enough; multi-client live collaboration is not
on its roadmap.

Out of scope for the server: the automation surface, the on-device
transform pipeline, and `tsc --noEmit` as a kernel op, which is kernel
work.
