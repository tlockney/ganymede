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

const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
};

/** Takes `out/.lock` (holding this pid); throws if a live process holds it. Returns a release function. */
export function lock(out) {
  const file = path.join(out, ".lock");
  for (let i = 0; i < 2; i++) {
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
      const pid = Number(fs.readFileSync(file, "utf8"));
      if (pid && alive(pid)) throw new Error(`${out} is in use by process ${pid} (${file})`);
      fs.unlinkSync(file);
    }
  }
  throw new Error(`could not lock ${out}`);
}

/**
 * Entries that were made with another prompt or review model than this run's.
 * Resuming on top of them would mix two measurements.
 */
export function mismatched(entries, { promptSha, model }) {
  return entries.filter((e) => (e.promptSha && e.promptSha !== promptSha) || (e.model && e.model !== model));
}
