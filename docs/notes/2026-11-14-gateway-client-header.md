## 2026-11-14, the gateway names itself as a program (#816)

Part of step 5 of making Ganymede Tailscale-aware: remote kernels without
a token. The gateway now sends `Ganymede-Client: ganymede-gateway` on
every REST request and on the kernel websocket handshake. A remote
Ganymede server that lets this machine in by tailnet identity (through
`tailscale serve`, or `whois` on its tailnet address) then accepts it with
no `gateway_token`, since the header marks a request no web page could
send. Other servers ignore the header.

Test: `requests_name_the_gateway_as_a_program` reads the request a
gateway sends to a bare listener. The websocket handshake sets the same
header; the end-to-end check is by hand on a tailnet (#816).
