## 2026-11-03, grouped `ganymede --help` (#320)

The top-level help listed fifteen commands in no order, with summaries
that ran to two or three wrapped lines, and ended in one long paragraph
of exit statuses. It is now grouped (Run code, Notebooks, Servers,
Setup), coloured as cargo's is, and ends with examples and an
exit-status table.

- clap derive splits a doc comment at the first blank line: the first
  paragraph is the summary that command lists show (`about`), the rest
  shows only in that command's `--help` (`long_about`). A one-paragraph
  doc comment is both, which is how the long summaries got into the
  lists. Write the first paragraph as one short line.
- clap 4 has no per-subcommand headings, so `help.rs` builds the
  top-level template at run time from `GROUPS`, taking each summary
  from the command itself. `main` parses with `help::command()`
  instead of `Cli::parse()`; the subcommands keep clap's own layout.
- Tests: every command is in exactly one group, and every summary in
  the command tree fits in 66 characters, so a new command or a long
  summary fails CI rather than the layout.
- Indented lines in a doc comment are reflowed into the paragraph;
  examples that must keep their lines go in `after_long_help`.
- ANSI codes written into the template are stripped like clap's own
  when stdout is not a terminal, so piped help stays plain.
