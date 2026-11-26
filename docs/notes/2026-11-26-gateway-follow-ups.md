## 2026-11-26, Gateway follow-ups from #969 (#970)

Two findings the review of #969 left for later.

The docs now say that without `default_kernel`, the `default` in
`/api/kernelspecs` is the first spec in the combined order (the gateway's
by name, then this machine's), not the first JSON key, which is sorted.
With a gateway set, that is this machine's first until the gateway first
answers with specs and the gateway's first after, so the kernel a new
notebook gets can depend on timing. Setting
`default_kernel` pins it. Both the Gateway section of the reference and
"Kernels on another server" say so.

The gateway status test checked that the token never shows up in the
error, but the token travels in the `Authorization` header and the stub's
503 never repeated it, so the check couldn't fail. The stub now reads the
whole request head and echoes its `Authorization` line in its error message, the test asserts the
echo arrived, and the token check proves that `scrub` takes the token out:
with the token left out of `scrub`, the test fails.
