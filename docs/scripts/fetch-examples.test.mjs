import { test } from "node:test";
import assert from "node:assert/strict";
import { render, slugOf } from "./fetch-examples.mjs";

const nb = (cells) => ({ metadata: { language_info: { name: "typescript" } }, cells });

test("slugOf", () => {
  assert.equal(slugOf("03 Charts and Diagrams.ipynb"), "03-charts-and-diagrams");
});

test("Markdown cells are kept from Vue, and the source note follows the title", () => {
  const { markdown } = render(nb([{ cell_type: "markdown", source: ["# Title\n", "{{ x }}"] }]), "01 A.ipynb");
  assert.match(markdown, /^<div v-pre>\n\n# Title\n\n<\/div>\n\n> \[!NOTE\]/);
  assert.match(markdown, /<div v-pre>\n\n\{\{ x \}\}\n\n<\/div>\n$/);
  assert.match(markdown, /\[The notebook\]\(https:\/\/github\.com\/tlockney\/ganymede-ui\/blob\/main\/examples\/01%20A\.ipynb\)/);
});

test("outputs", () => {
  const { markdown, files } = render(
    nb([
      { cell_type: "markdown", source: "# T" },
      {
        cell_type: "code",
        source: "x",
        outputs: [
          { output_type: "stream", text: ["a\n"] },
          { output_type: "error", ename: "E", evalue: "v", traceback: ["\x1b[31mE: v\x1b[0m"] },
          { output_type: "execute_result", data: { "image/svg+xml": "<svg/>", "text/plain": "x" } },
          { output_type: "display_data", data: { "image/png": "aGk=\n" } },
          { output_type: "display_data", data: { "text/html": "<b>1</b>\n\n<i>2</i>" } },
          { output_type: "display_data", data: { "text/html": "<div></div><script>alert(1)</script>" } },
          { output_type: "display_data", data: { "application/vnd.jupyter.widget-view+json": { model_id: "m" } } },
          { output_type: "execute_result", data: { "text/plain": "```" } },
        ],
      },
    ]),
    "02 B.ipynb",
  );
  assert.match(markdown, /```ts\nx\n```/);
  assert.match(markdown, /```text\na\n```/);
  assert.match(markdown, /```text\nE: v\n```/);
  assert.deepEqual([...files.keys()], ["images/02-b-1.svg", "images/02-b-2.png"]);
  assert.equal(files.get("images/02-b-2.png").toString(), "hi");
  assert.match(markdown, /<img src="\.\/images\/02-b-1\.svg"/);
  assert.match(markdown, /<div class="nb-output" v-pre>\n<b>1<\/b>\n<i>2<\/i>\n<\/div>/);
  assert.match(markdown, /sandbox="allow-scripts"[^>]*srcdoc="&lt;div&gt;&lt;\/div&gt;&lt;script&gt;alert\(1\)&lt;\/script&gt;"/);
  assert.match(markdown, /::: info Widget/);
  assert.match(markdown, /````text\n```\n````/);
});
