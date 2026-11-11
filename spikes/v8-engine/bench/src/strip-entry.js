// What dist/strip-bundle.js defines: cdp-kernel's sucrase type stripper
// (src/strip-sucrase.ts) reduced to its work, as a classic script any
// engine can load.
import { parse } from "acorn";
import { transform } from "sucrase";

globalThis.benchStrip = (code) => {
  let result;
  try {
    result = transform(code, {
      transforms: ["typescript", "jsx"],
      jsxRuntime: "classic",
      production: true,
      keepUnusedImports: true,
      disableESTransforms: true,
      filePath: "cell.tsx",
      sourceMapOptions: { compiledFilename: "cell.tsx" },
    });
  } catch {
    result = transform(code, {
      transforms: ["typescript"],
      keepUnusedImports: true,
      disableESTransforms: true,
      filePath: "cell.tsx",
      sourceMapOptions: { compiledFilename: "cell.tsx" },
    });
  }
  parse(result.code, { ecmaVersion: "latest", sourceType: "module", allowAwaitOutsideFunction: true });
  return result.code;
};
