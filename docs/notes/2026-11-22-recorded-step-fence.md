## 2026-11-22, Recorded steps keep their commands in code blocks (#930)

Thomas opened a journal on the iPad and saw a recorded step as four
lines of H1 text. The agent had added the step with
`cell_add(mode: "recorded")` itself and written the shell session bare:
its `# comment` lines are Markdown headings. The journal hook never had
this problem, because `recordedSource` fences the command (```` ```sh ````)
and the output (```` ```text ````); only steps an agent writes by hand did.
The apps render the Markdown correctly, so the fix belongs where the
source is written.

Two parts. `cell_add`'s descriptions, the journal prompt and the journal
skill now say a recorded step's commands and output go in fenced code
blocks. And the server fences a recorded Markdown source that has no
fenced block of its own in ```` ```text ````, with a fence longer than any
backtick run inside it, so a step written bare reads as the session it
was. A source with any fence (backticks or tildes, at most three spaces
in, as CommonMark allows) is kept as written, as are notes, code and raw
cells, and an empty source. `cell_edit` fences a recorded Markdown
step the same way, since the journal prompt tells agents to fix a cell
in place rather than add another. The server doesn't try to tell prose from
commands: a bare step that was all prose becomes a text block, which
still reads, where a guess could turn a comment into a heading again.

The check for a fence is simple on purpose. A fence inside a list item
or a blockquote (`- ```sh`, `> ```sh`) isn't seen, so such a step is
wrapped whole and shows its Markdown as text; and a bare session whose
output happens to print a line starting with three backticks counts as
fenced and is kept bare. Both still read, and neither is what agents
write when they follow the instructions.
