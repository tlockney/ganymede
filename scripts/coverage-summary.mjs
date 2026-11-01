// Turns `cargo llvm-cov report --json --summary-only` into Markdown for
// the job summary: total line coverage, a row per crate, and a row per
// changed .rs file, so a reviewer sees which new code no test ran.
//
// Usage: node scripts/coverage-summary.mjs <cov.json> [changed-file...]
// Changed files are repo-relative paths; ones not in the report (deleted,
// or not Rust) are left out.
import fs from 'node:fs';
import path from 'node:path';

const [report, ...changed] = process.argv.slice(2);
if (!report) {
  console.error('usage: coverage-summary.mjs <cov.json> [changed-file...]');
  process.exit(2);
}
const data = JSON.parse(fs.readFileSync(report, 'utf8')).data[0];
const root = process.cwd() + path.sep;
const rel = f => (f.startsWith(root) ? f.slice(root.length) : f);
const pct = (covered, count) => (count ? ((100 * covered) / count).toFixed(1) + '%' : '–');

const files = data.files.map(f => ({ name: rel(f.filename), lines: f.summary.lines }));
const crates = new Map();
for (const f of files) {
  const m = f.name.match(/^crates\/([^/]+)\//);
  const key = m ? m[1] : '(other)';
  const c = crates.get(key) ?? { count: 0, covered: 0 };
  c.count += f.lines.count;
  c.covered += f.lines.covered;
  crates.set(key, c);
}

const t = data.totals.lines;
const out = [];
out.push(`### Line coverage: ${pct(t.covered, t.count)} (${t.covered} of ${t.count} lines)`);
out.push('');
out.push('Informational, not a gate. Unit, integration and e2e tests on Linux with cdp-kernel and ipykernel.');
out.push('');

const byName = new Map(files.map(f => [f.name, f]));
const touched = [...new Set(changed)].filter(n => byName.has(n)).sort();
if (touched.length) {
  out.push('#### Changed files');
  out.push('');
  out.push('| File | Lines | Covered | Missed |');
  out.push('| --- | ---: | ---: | ---: |');
  for (const n of touched) {
    const l = byName.get(n).lines;
    out.push(`| \`${n}\` | ${l.count} | ${pct(l.covered, l.count)} | ${l.count - l.covered} |`);
  }
  out.push('');
}

out.push('<details><summary>By crate</summary>');
out.push('');
out.push('| Crate | Lines | Covered |');
out.push('| --- | ---: | ---: |');
for (const [name, c] of [...crates].sort(([a], [b]) => a.localeCompare(b))) {
  out.push(`| ${name} | ${c.count} | ${pct(c.covered, c.count)} |`);
}
out.push('');
out.push('</details>');
console.log(out.join('\n'));
