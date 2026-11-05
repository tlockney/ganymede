# Privacy and crash reporting

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release. Crash reporting is being added
> ([#384](https://github.com/tlockney/ganymede/issues/384)); until a
> build has it, Ganymede sends nothing.

Ganymede is local-first. Your notebooks, files and kernel output stay on
your machine. Nothing is sent anywhere unless you turn on crash
reporting, which is off by default everywhere.

## What crash reporting is for

When you turn it on, a crash in Ganymede's own code (a panic in the
server or CLI, an unhandled error in the app) sends a report to
[Sentry](https://sentry.io), so it can be fixed without you filing a bug.
Errors in your own code, such as an exception in a cell or a kernel that
exits, are not crashes and are never reported.

## What is sent

- The stack trace of the crash, in Ganymede's code.
- The error's type and message.
- The Ganymede version, and whether it is a release or a `main` build.
- The operating system, its version, and the CPU architecture.

## What is never sent

- Notebook or file contents, cell source, or kernel output.
- File paths under your home directory: they are rewritten to start
  with `~` before a report leaves your machine.
- Environment variables, host names, or IP addresses.
- Request bodies, local variables, screenshots, or session replays.

These rules are applied on your machine, before a report is sent, not
only in Sentry's settings.

## Turning it on and off

### Server and CLI

Off unless you turn it on. Either set it in `ganymede.toml`:

```toml
crash_reporting = true
```

or set `GANYMEDE_CRASH_REPORTING=1` in the environment, which overrides
the config file; `GANYMEDE_CRASH_REPORTING=0` turns it off. Only release
binaries, from `install.sh` or a GitHub release, can send reports; a
build from source can't unless it enables the `crash-reporting` cargo
feature.

### Mac, iPad and iPhone apps

The app asks once whether to send crash reports, with "Don't send" as
the default. Change your answer at any time in Settings.

### Browser app

Off unless you turn it on in the app's settings.

### cdp-kernel

cdp-kernel's own documentation says how it reports crashes in the
kernel itself.
