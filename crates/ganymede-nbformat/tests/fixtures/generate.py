"""Regenerate the test fixtures in this directory with the reference Python nbformat.

Usage (from the crate directory, with a venv that has nbformat installed):

    python tests/fixtures/generate.py

Every fixture that ends in `.ipynb` and is listed in NBFORMAT_WRITTEN below is
produced by `nbformat.write`, which is the byte-for-byte target of the Rust
writer. `unsplit_source.ipynb` is deliberately written with plain `json.dumps`
(no `split_lines`, unsorted keys, indent 2) to exercise lenient reading;
`unsplit_source.expected.ipynb` is what `nbformat.write` produces after reading
it back, and is the expected output of the Rust writer for that input.

Cell ids are passed explicitly so the output is deterministic.
"""

from __future__ import annotations

import base64
import json
import pathlib

import nbformat
from nbformat.v4 import (
    new_code_cell,
    new_markdown_cell,
    new_notebook,
    new_output,
    new_raw_cell,
)

HERE = pathlib.Path(__file__).resolve().parent

PNG_1X1 = base64.b64encode(
    bytes.fromhex(
        "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c489"
        "0000000d49444154789c6360f8cfc00000030101009a1c4e5c0000000049454e44ae426082"
    )
).decode("ascii")

SVG = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">\n'
    '  <rect width="10" height="10" fill="#f00"/>\n'
    "</svg>\n"
)

ANSI_TRACEBACK = [
    "\x1b[0;31m---------------------------------------------------------------------------\x1b[0m",
    "\x1b[0;31mZeroDivisionError\x1b[0m                         Traceback (most recent call last)",
    "Cell \x1b[0;32mIn[3], line 1\x1b[0m\n\x1b[0;32m----> 1\x1b[0m \x1b[38;5;241m1\x1b[39m \x1b[38;5;241m/\x1b[39m \x1b[38;5;241m0\x1b[39m\n",
    "\x1b[0;31mZeroDivisionError\x1b[0m: division by zero",
]

COMMON_METADATA = {
    "kernelspec": {
        "display_name": "Python 3 (ipykernel)",
        "language": "python",
        "name": "python3",
    },
    "language_info": {
        "codemirror_mode": {"name": "ipython", "version": 3},
        "file_extension": ".py",
        "mimetype": "text/x-python",
        "name": "python",
        "nbconvert_exporter": "python",
        "pygments_lexer": "ipython3",
        "version": "3.13.0",
    },
    # Keys deliberately out of order and with a nested structure, to exercise
    # recursive key sorting.
    "zzz_custom": {"b": [1, 2, {"y": 1, "x": 2}], "a": None, "unicode_key_é": True},
    "Capital": "sorts before lowercase by code point",
    # Floats whose repr differs between Python and a naive shortest-float
    # printer (exponent layout), plus integers at the i64/u64 edges.
    "numbers": [1e-5, 2.5e-7, 0.0001, 1e15, 1e16, 1.5e300, -0.0, 0.1 + 0.2, 123456789.125, -1, 2**63 - 1, 2**64 - 1],
    # 17 significant digits: a reader that is not correctly rounded parses
    # some of these to a neighbouring double (#72).
    "precise_floats": [123456789.12345679, 0.030000000000000002, 1.1499999999999999e300, 2.2250738585072014e-308, 5e-324, 1.7976931348623157e308, 0.1234567890123456789, 9007199254740993.0],
    # Beyond u64: Python keeps the exact int.
    "big_ints": [2**64, -(2**64) - 1, 10**30],
}


def code_outputs() -> nbformat.NotebookNode:
    cells = [
        new_code_cell(
            id="imports",
            source="import sys\nimport time\n\nprint(sys.version)\n",
            execution_count=1,
            outputs=[
                new_output("stream", name="stdout", text="3.13.0 (main) [GCC 13.2.0]\n"),
            ],
        ),
        new_code_cell(
            id="progress",
            source=(
                "for i in range(3):\n"
                "    print(f'\\rprogress: {i*50}%', end='')\n"
                "    time.sleep(0.1)\n"
                "print()\n"
                "print('warn', file=sys.stderr)"
            ),
            execution_count=2,
            outputs=[
                # \r progress bar text: splitlines must break on \r as well as \n.
                new_output(
                    "stream",
                    name="stdout",
                    text="\rprogress: 0%\rprogress: 50%\rprogress: 100%\n",
                ),
                new_output("stream", name="stderr", text="warn\n"),
            ],
        ),
        new_code_cell(
            id="result",
            source="df.describe()",
            execution_count=3,
            metadata={"collapsed": False, "scrolled": True, "tags": ["b-tag", "a-tag"]},
            outputs=[
                new_output(
                    "execute_result",
                    execution_count=3,
                    data={
                        "text/plain": "   a  b\n0  1  2\n1  3  4",
                        "text/html": (
                            "<div>\n<table border=\"1\">\n  <tr><th>a</th><th>b</th></tr>\n"
                            "  <tr><td>1</td><td>2</td></tr>\n</table>\n</div>"
                        ),
                        # JSON mimes are stored as JSON and must NOT be split.
                        "application/json": {
                            "rows": [{"a": 1, "b": 2}, {"a": 3, "b": 4}],
                            "y": [0.30000000000000004, 0.7071067811865476, 2.718281828459045, 1e-07],
                            "note": "multi\nline string inside json stays a string",
                        },
                        "application/vnd.dataresource+json": {
                            "schema": {"fields": [{"name": "a", "type": "integer"}]},
                            "data": [{"a": 1}],
                        },
                        # A string-valued non-text mime that is also NOT split.
                        "application/vnd.custom": "line one\nline two\n",
                    },
                    metadata={"application/json": {"expanded": True}},
                ),
            ],
        ),
        new_code_cell(
            id="display",
            source="display(img)\ndisplay(svg)\ndisplay(js)",
            execution_count=4,
            outputs=[
                new_output(
                    "display_data",
                    data={
                        "image/png": PNG_1X1,
                        "text/plain": "<PIL.Image.Image image mode=RGBA size=1x1>",
                    },
                    metadata={"image/png": {"width": 1, "height": 1}, "needs_background": "light"},
                ),
                new_output(
                    "display_data",
                    data={"image/svg+xml": SVG, "text/plain": "<IPython.core.display.SVG object>"},
                    metadata={},
                ),
                new_output(
                    "display_data",
                    data={
                        "application/javascript": "console.log('a');\nconsole.log('b');\n",
                        "text/plain": "<IPython.core.display.Javascript object>",
                    },
                    metadata={},
                ),
            ],
        ),
        new_code_cell(
            id="error",
            source="1 / 0",
            execution_count=5,
            outputs=[
                new_output(
                    "error",
                    ename="ZeroDivisionError",
                    evalue="division by zero",
                    traceback=ANSI_TRACEBACK,
                ),
            ],
        ),
        new_code_cell(
            id="exotic-line-breaks",
            # Every separator that str.splitlines() recognises, plus a bare \r
            # and a \r\n pair. The Rust splitlines must match this exactly.
            source=(
                "a\n"
                "b\r"
                "c\r\n"
                "d\x0b"
                "e\x0c"
                "f\x1c"
                "g\x1d"
                "h\x1e"
                "i\x85"
                "j "
                "k "
                "l\n\n"
                "no trailing newline"
            ),
            execution_count=None,
            outputs=[],
        ),
        new_code_cell(id="empty-source", source="", execution_count=None, outputs=[]),
        new_code_cell(
            id="unknown-fields",
            source="pass",
            execution_count=6,
            outputs=[
                new_output("stream", name="stdout", text="x\n"),
            ],
        ),
    ]
    nb = new_notebook(cells=cells, metadata=dict(COMMON_METADATA))
    # Fields that are not in the schema. Strict validation rejects them (the
    # constructors above would raise), so they are added afterwards; nbformat
    # logs a validation error on write but writes them anyway. They must
    # survive a Rust round trip untouched.
    nb["x_extra_notebook_field"] = {"nested": {"z": 1, "a": 2}}
    nb.cells[-1]["x_extra_cell_field"] = "kept"
    nb.cells[-1].outputs[0]["x_extra_output_field"] = {"k": [1, "two", None]}
    return nb


def markdown_attachments() -> nbformat.NotebookNode:
    cells = [
        new_markdown_cell(
            id="intro",
            source=(
                "# Résumé — 日本語のノート 🚀\n"
                "\n"
                "Accents: café, naïve, Straße, Ærø.\n"
                "CJK: 中文, 한국어, 日本語。\n"
                "Emoji: 🧪 🦀 👩‍🔬 (with a ZWJ sequence)\n"
                "\n"
                "![a tiny image](attachment:dot.png)\n"
            ),
        ),
        new_markdown_cell(
            id="attached",
            source="Text attachment below.\n\n![t](attachment:notes.txt)",
            attachments={
                "dot.png": {"image/png": PNG_1X1},
                "notes.txt": {"text/plain": "first line\nsecond line\n"},
                "snippet.js": {"application/javascript": "let x = 1;\nlet y = 2;"},
                "diagram.svg": {"image/svg+xml": SVG},
                "data.json": {"application/json": {"a": [1, 2, 3], "s": "x\ny"}},
            },
            metadata={"tags": ["ünïcode"], "jp-MarkdownHeadingCollapsed": True},
        ),
        new_raw_cell(
            id="raw",
            source=".. note::\n\n   Raw reStructuredText with a tab\there.\n",
            metadata={"format": "text/restructuredtext"},
            attachments={"raw.txt": {"text/plain": "raw cells may have attachments too"}},
        ),
        new_code_cell(
            id="code-unicode",
            source="print('héllo wörld 🌍')",
            execution_count=1,
            outputs=[new_output("stream", name="stdout", text="héllo wörld 🌍\n")],
        ),
    ]
    nb = new_notebook(cells=cells, metadata={"title": "Ünïcödé", "authors": [{"name": "Zoë"}]})
    return nb


def empty() -> nbformat.NotebookNode:
    return new_notebook()


def unsplit_source() -> dict:
    """A hand-built notebook dict with sources and texts as single strings."""
    return {
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {
            "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"},
            # Transient keys: nbformat strips these on read and on write.
            "orig_nbformat": 4,
            "orig_nbformat_minor": 2,
            "signature": "sha256:deadbeef",
        },
        "cells": [
            {
                "cell_type": "markdown",
                "id": "md",
                "metadata": {"trusted": True},
                "source": "# Title\n\nParagraph one.\nParagraph two.\n",
                "attachments": {"a.txt": {"text/plain": "one\ntwo\n"}},
            },
            {
                "cell_type": "code",
                "id": "code",
                "metadata": {},
                "execution_count": 7,
                "source": "print('hi')\nprint('there')",
                "outputs": [
                    {"output_type": "stream", "name": "stdout", "text": "hi\nthere\n"},
                    {
                        "output_type": "execute_result",
                        "execution_count": 7,
                        "data": {
                            "text/plain": "'multi\\nline'",
                            "text/html": "<p>a</p>\n<p>b</p>",
                            "image/svg+xml": "<svg/>\n",
                            "application/json": {"k": "v\nw"},
                            # A list value under a non-JSON mime that is NOT a
                            # list of strings is left alone by rejoin_lines.
                            "application/vnd.mixed": ["a", 1],
                            # A list of strings under a non-text mime is
                            # rejoined on read and, being non-text, not split
                            # again on write: it comes out as a single string.
                            "application/vnd.lines": ["joined\n", "on read"],
                        },
                        "metadata": {},
                    },
                ],
            },
            {"cell_type": "raw", "id": "raw", "metadata": {}, "source": "raw\r\ntext\r"},
        ],
    }


def invalid_outputs() -> nbformat.NotebookNode:
    """Outputs that break the schema. nbformat logs a validation error on
    write but writes them as they are, and Ganymede must too (#73)."""
    return nbformat.from_dict({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "cell_type": "code",
                "id": "outputs",
                "metadata": {},
                "execution_count": 1,
                "source": "show()",
                "outputs": [
                    {"output_type": "future_output", "payload": {"b": 1, "a": [1, 2]}},
                    {"output_type": "stream", "name": 5, "text": "invalid name\n"},
                    {"output_type": "display_data", "data": {"text/plain": "x\ny"}, "metadata": "not a map"},
                    {"output_type": "error", "ename": "E", "evalue": "v", "traceback": "not a list"},
                    {"output_type": "stream", "name": "stdout", "text": "valid\n"},
                ],
            },
        ],
    })


def lenient() -> dict:
    """A notebook nbformat reads (with logged validation errors) that a strict
    typed reader would reject (#73). Written as plain JSON with
    `ensure_ascii=True`, so the lone surrogate becomes a `\\ud800` escape;
    `nbformat.write` could not write it to UTF-8 at all."""
    return {
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": None,
            },
            {
                "cell_type": "code",
                "id": "dup",
                "metadata": {},
                "execution_count": 1.0,
                "source": "print(1)",
                "outputs": [
                    {"output_type": "stream", "text": "no name\n"},
                    {"output_type": "error", "traceback": []},
                    {"output_type": "execute_result", "execution_count": -1, "data": {"text/plain": "1"}, "metadata": None},
                ],
            },
            {
                "cell_type": "code",
                "id": "dup",
                "metadata": {},
                "execution_count": -1,
                "source": ["x = ", "1"],
                "outputs": [],
            },
            {
                "cell_type": "code",
                "id": "plain",
                "metadata": {},
                "execution_count": None,
                "source": "",
                "outputs": [],
            },
            {
                "cell_type": "markdown",
                "id": "surrogates",
                "metadata": {},
                # A lone high surrogate, a lone low one, a valid pair (🚀), and
                # an escaped backslash followed by text that looks like an escape.
                "source": "lone \ud800 and \udc00, pair \U0001f680, literal \\ud800",
            },
        ],
    }


NBFORMAT_WRITTEN = {
    "code_outputs.ipynb": code_outputs,
    "markdown_attachments.ipynb": markdown_attachments,
    "empty.ipynb": empty,
    "invalid_outputs.ipynb": invalid_outputs,
}


def main() -> None:
    for name, build in NBFORMAT_WRITTEN.items():
        nb = build()
        nbformat.write(nb, HERE / name)
        print("wrote", name)

    unsplit = unsplit_source()
    with (HERE / "unsplit_source.ipynb").open("w", encoding="utf8") as f:
        json.dump(unsplit, f, indent=2, ensure_ascii=False)
        f.write("\n")
    print("wrote unsplit_source.ipynb")

    nb = nbformat.read(HERE / "unsplit_source.ipynb", as_version=nbformat.NO_CONVERT)
    nbformat.write(nb, HERE / "unsplit_source.expected.ipynb")
    print("wrote unsplit_source.expected.ipynb")

    with (HERE / "lenient.ipynb").open("w", encoding="utf8") as f:
        json.dump(lenient(), f, indent=1, ensure_ascii=True)
        f.write("\n")
    print("wrote lenient.ipynb")


if __name__ == "__main__":
    main()
