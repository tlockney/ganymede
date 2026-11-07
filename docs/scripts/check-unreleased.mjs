// Docs for a feature the latest release doesn't have carry the standard
// note (CLAUDE.md, #408):
//
//   > [!NOTE]
//   > **Unreleased.** Available only from a build of `main` (HEAD), ...
//
// The release finds these notes by grepping for "Unreleased\." and removes
// them, so any other way of saying it, such as an inline "(unreleased)",
// ships still saying so. This fails on any "unreleased" outside such a
// note. releasing.md is skipped: it describes the notes.
//
// Text about the apps, which ship from ganymede-ui, uses
// **Unreleased in ganymede-ui.** instead: Ganymede's release leaves it,
// and ganymede-ui's release lists it (#502). In site/index.html either
// form may sit in an HTML comment, as a marker that renders nothing.
import { readdirSync, readFileSync, existsSync } from 'node:fs';

const root = new URL('../../', import.meta.url);
const files = ['README.md', ...readdirSync(new URL('docs/', root)).filter((f) => f.endsWith('.md') && f !== 'releasing.md').map((f) => `docs/${f}`), 'site/index.html'].filter((f) => existsSync(new URL(f, root)));
const problems = [];

for (const file of files) {
  let fenced = false;
  readFileSync(new URL(file, root), 'utf8')
    .split('\n')
    .forEach((line, i) => {
      if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
      if (fenced) return;
      // Code spans quote the note's words rather than make a note.
      const text = line.replace(/`[^`]*`/g, '');
      if (!/unreleased/i.test(text)) return;
      const where = `${file}:${i + 1}`;
      if (!text.includes('**Unreleased.**') && !text.includes('**Unreleased in ganymede-ui.**')) {
        problems.push(`${where}: say "unreleased" with the standard note, not inline`);
      } else if (file.endsWith('.md') && !/^\s*>/.test(text)) {
        problems.push(`${where}: put the note in a "> [!NOTE]" block`);
      }
    });
}

if (problems.length > 0) {
  console.error(`Unreleased notes (CLAUDE.md, "Documenting unreleased features"):\n  ${problems.join('\n  ')}`);
  process.exit(1);
}
