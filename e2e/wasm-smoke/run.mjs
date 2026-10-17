// Runs the WASI build of the smoke test under Node's WASI.
// Usage: node run.mjs <path to ganymede-wasm-smoke.wasm>
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";
import { argv, exit } from "node:process";

const wasi = new WASI({ version: "preview1", args: ["smoke"], env: {}, returnOnExit: true });
const module = await WebAssembly.compile(await readFile(argv[2]));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
exit(wasi.start(instance));
