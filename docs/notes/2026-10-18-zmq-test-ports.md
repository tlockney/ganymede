## 2026-10-18, ZMQ tests bind port 0 (#120)

The ZMQ tests picked ports by binding `127.0.0.1:0` with a std listener,
dropping it, and binding the port later. Under `cargo test`'s parallel
threads another test could take the port in between, and CI hit
`AddrInUse`. The test kernels now bind `tcp://127.0.0.1:0` themselves and
build the connection info from the ports `bind` returns. The same fix
applies to any future test that needs a listening port: let the socket
that will listen pick it.
