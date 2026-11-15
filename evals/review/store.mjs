// results.json in an out directory: read strictly, written atomically,
// and locked so two invocations don't both write it.

import fs from "node:fs";
import path from "node:path";

/** All entries; [] only when the file doesn't exist. A file that doesn't parse is an error, not a fresh start. */
export function readResults(out) {
  const file = path.join(out, "results.json");
  let text;
  try {
    text = fs.readFileSync(file, "utf8");
  } catch (e) {
    if (e.code === "ENOENT") return [];
    throw e;
  }
  let v;
  try {
    v = JSON.parse(text);
  } catch (e) {
    throw new Error(`${file} does not parse (${e.message}); fix or remove it, it holds finished runs`);
  }
  if (!Array.isArray(v)) throw new Error(`${file} is not an array`);
  return v;
}

/** Writes through a temp file and a rename, so a crash never leaves half a file. */
export function writeResults(out, entries) {
  const file = path.join(out, "results.json");
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(entries, null, 2) + "\n");
  fs.renameSync(tmp, file);
}

export const entryKey = (e) => `${e.case}|${e.lens ?? ""}|${e.run}`;

export const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
};

/**
 * Takes `out/.lock` (holding this pid); throws if a live process holds it.
 * Returns a release function. A dead holder's lock is taken over by renaming
 * it to a name of our own (only one process can rename it), then checking it
 * still holds the dead pid: if it was a fresh lock another process just took,
 * it is linked back and we give up.
 */
export function lock(out) {
  const file = path.join(out, ".lock");
  for (let i = 0; i < 4; i++) {
    try {
      fs.writeFileSync(file, String(process.pid), { flag: "wx" });
      const release = () => {
        try {
          if (fs.readFileSync(file, "utf8") === String(process.pid)) fs.unlinkSync(file);
        } catch {}
      };
      process.on("exit", release);
      return release;
    } catch (e) {
      if (e.code !== "EEXIST") throw e;
    }
    let text;
    try {
      text = fs.readFileSync(file, "utf8");
    } catch {
      continue; // released between our attempt and the read
    }
    const pid = Number(text);
    if (pid && alive(pid)) throw new Error(`${out} is in use by process ${pid} (${file})`);
    const mine = `${file}.stale.${process.pid}`;
    try {
      fs.renameSync(file, mine);
    } catch {
      continue; // another process took it over first
    }
    const moved = fs.readFileSync(mine, "utf8");
    if (moved === text) fs.unlinkSync(mine);
    else {
      try { fs.linkSync(mine, file); } catch {}
      fs.unlinkSync(mine);
      throw new Error(`${out} is in use by process ${moved} (${file})`);
    }
  }
  throw new Error(`could not lock ${out}`);
}

/**
 * Entries that were made with another prompt, review model or judge model
 * than this run's. Resuming on top of them would mix two measurements.
 */
export function mismatched(entries, { promptSha, model, judgeModel }) {
  return entries.filter((e) => (e.promptSha && e.promptSha !== promptSha) || (e.model && e.model !== model) || (e.judgeModel && e.judgeModel !== judgeModel));
}

/**
 * For `--redo`: the entries to keep. The selected runs are replaced by the
 * redo, and a mismatched entry outside the selection is dropped, so the
 * report never mixes two prompts. `selected` is a set of entry keys.
 */
export function afterRedo(entries, current, selected) {
  const bad = new Set(mismatched(entries, current));
  return entries.filter((e) => !selected.has(entryKey(e)) && !bad.has(e));
}
