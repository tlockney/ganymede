// Builds what the runners load besides the workloads:
//   dist/strip-bundle.js  sucrase + acorn as one classic script (ts-strip)
//   dist/kernel/          cdp-kernel's browser kernel, as the apps load it
// Run from this directory: node build.mjs
import { cpSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { build } from "esbuild";

const require = createRequire(import.meta.url);

await build({
  entryPoints: ["src/strip-entry.js"],
  outfile: "dist/strip-bundle.js",
  bundle: true,
  format: "iife",
  platform: "neutral",
  mainFields: ["module", "main"],
  target: "es2022",
  logLevel: "warning",
});

// cdp-kernel's package exports ./browser and ./browser/worker; copy the
// whole dist/browser folder so the kernel's own imports resolve.
const kernelDir = dirname(require.resolve("@tlockney/cdp-kernel/browser/worker"));
mkdirSync("dist", { recursive: true });
cpSync(kernelDir, join("dist", "kernel"), { recursive: true });
console.log("built dist/strip-bundle.js and dist/kernel/");
