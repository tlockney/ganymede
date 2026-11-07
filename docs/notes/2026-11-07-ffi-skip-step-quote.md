## 2026-11-07, quote the ffi job's skip message (#482)

#471's skip step, `run: echo "... (#418, #457)."`, was a plain YAML
scalar, where ` #` starts a comment. The shell got `echo "... (#418,`
and failed, so every PR whose `ffi` cell skipped the build went red.
The scalar is quoted now. `run:` values that mention issue numbers need
quotes or a block scalar (`|`).
