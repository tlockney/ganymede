## 2026-11-07, kernelspec env values are template-expanded (#438)

jupyter_client runs each kernelspec `env` value through
`string.Template(v).safe_substitute(env)`, where `env` is the server's
environment plus the launch's. Ganymede passed the values as written, so
`"PATH": "/opt/conda/envs/x/bin:${PATH}"` gave the kernel a literal
`${PATH}` and it couldn't find system commands.

`SubprocessProvisioner::launch` now expands values with `expand_env`,
which follows `safe_substitute`: `$NAME` and `${NAME}` (an ASCII letter or
`_`, then letters, digits, `_`), `$$` for `$`, and unknown names or a bare
`$` left as written. Names come from the launch's env first, then the
host's. The kernelspec's values still override the host's, and the
launch's override both, as before.
