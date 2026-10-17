"""Regenerate trust.json, the notebook-signing fixtures, with the reference nbformat.

Usage (from the crate directory, with a venv that has nbformat installed):

    python tests/fixtures/generate_trust.py

For each notebook, `signatures` holds `NotebookNotary.compute_signature` over
the notebook as `nbformat.read` returns it (what jupyter_server hashes when it
serves a notebook), for two secrets: a short ASCII one and the base64 text with
newlines that `NotebookNotary` generates for `notebook_secret`. The `raw` case
hashes a dict that still has `metadata.signature` and cell `trusted` flags,
which `compute_signature` must skip and keep, respectively. `check_cells`
records `NotebookNotary.check_cells` on raw dicts and the dict it leaves
behind (it pops `trusted` from code cells).
"""

from __future__ import annotations

import copy
import json
import pathlib

import nbformat
from nbformat.sign import MemorySignatureStore, NotebookNotary
from nbformat.v4 import new_code_cell, new_markdown_cell, new_notebook, new_output

HERE = pathlib.Path(__file__).resolve().parent
NBFORMAT_FIXTURES = HERE.parent.parent.parent / "ganymede-nbformat" / "tests" / "fixtures"

SECRETS = {
    "short": b"ganymede-test-secret",
    # The shape `NotebookNotary` writes: base64.encodebytes of random bytes,
    # 76-character lines, each ending in a newline.
    "base64": b"".join(
        (b"QUJD" * 19)[:76] + b"\n" for _ in range(3)
    ),
}


def notary(secret: bytes) -> NotebookNotary:
    return NotebookNotary(
        secret=secret, store_factory=MemorySignatureStore, data_dir="", db_file=":memory:"
    )


def signing_notebook():
    nb = new_notebook(
        metadata={
            "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
            "numbers": {
                "int": 7,
                "neg": -3,
                "big": 12345678901234567,
                "float": 1.5,
                "whole_float": 2.0,
                "tiny": 1e-05,
                "huge": 1e16,
                "third": 0.1 + 0.2,
            },
            "flags": [True, False, None],
            "unicode é中": "café \U0001f600",
            "Zed": "upper-case keys sort before lower-case",
            "empty": {},
        }
    )
    nb.cells = [
        new_markdown_cell("# Title\n\nSome *markdown*.", id="md-1"),
        new_code_cell(
            "from IPython.display import HTML\nHTML('<b>hi</b>')",
            id="code-1",
            execution_count=1,
            outputs=[
                new_output(
                    "execute_result",
                    data={"text/html": "<b>hi</b>", "text/plain": "<IPython.core.display.HTML object>"},
                    execution_count=1,
                ),
                new_output(
                    "display_data",
                    data={
                        "application/javascript": "console.log(1);\nconsole.log(2);",
                        "application/json": {"a": [1, 2.5, None, True]},
                        "image/svg+xml": "<svg>\n</svg>",
                    },
                    metadata={"isolated": True},
                ),
                new_output("stream", name="stdout", text="line 1\nline 2\n"),
            ],
        ),
        new_code_cell("1/0", id="code-2", metadata={"tags": ["raises-exception"]}),
    ]
    return nb


CHECK_CELLS_CASES = {
    "no code cells": {"cells": [{"cell_type": "markdown", "metadata": {}, "source": "x"}]},
    "streams and errors only": {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {},
                "outputs": [
                    {"output_type": "stream", "name": "stdout", "text": "x"},
                    {"output_type": "error", "ename": "E", "evalue": "v", "traceback": []},
                ],
            }
        ]
    },
    "rich output, untrusted": {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {},
                "outputs": [{"output_type": "display_data", "data": {"text/html": "<b>x</b>"}, "metadata": {}}],
            }
        ]
    },
    "rich output, trusted flag": {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {"trusted": True},
                "outputs": [{"output_type": "display_data", "data": {"text/html": "<b>x</b>"}, "metadata": {}}],
            }
        ]
    },
    "rich output, trusted false": {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {"trusted": False},
                "outputs": [{"output_type": "execute_result", "data": {}, "execution_count": 1, "metadata": {}}],
            }
        ]
    },
    "only safe keys": {
        "cells": [
            {
                "cell_type": "code",
                "metadata": {},
                "outputs": [{"output_type": "execute_result", "execution_count": 1, "metadata": {}}],
            }
        ]
    },
    "truthy trusted values": {
        "cells": [
            {"cell_type": "code", "metadata": {"trusted": 1}, "outputs": [{"output_type": "display_data", "data": {}}]},
            {"cell_type": "code", "metadata": {"trusted": "yes"}, "outputs": [{"output_type": "display_data", "data": {}}]},
        ]
    },
    "falsy trusted values": {
        "cells": [
            {"cell_type": "code", "metadata": {"trusted": 0}, "outputs": []},
            {"cell_type": "code", "metadata": {"trusted": ""}, "outputs": [{"output_type": "display_data", "data": {}}]},
        ]
    },
    "one untrusted cell among trusted": {
        "cells": [
            {"cell_type": "code", "metadata": {"trusted": True}, "outputs": [{"output_type": "display_data", "data": {}}]},
            {"cell_type": "code", "metadata": {}, "outputs": [{"output_type": "display_data", "data": {}}]},
            {"cell_type": "code", "metadata": {"trusted": True}, "outputs": []},
            {"cell_type": "markdown", "metadata": {"trusted": True}, "source": ""},
        ]
    },
}


def main():
    signing = signing_notebook()
    nbformat.write(signing, HERE / "signing.ipynb")

    notebooks = ["signing.ipynb"]
    sources = {"signing.ipynb": HERE / "signing.ipynb"}
    for name in ["code_outputs.ipynb", "markdown_attachments.ipynb", "empty.ipynb"]:
        key = f"../../../ganymede-nbformat/tests/fixtures/{name}"
        notebooks.append(key)
        sources[key] = NBFORMAT_FIXTURES / name

    signatures = []
    for name in notebooks:
        nb = nbformat.read(sources[name], as_version=4)
        signatures.append(
            {
                "notebook": name,
                "signatures": {
                    label: notary(secret).compute_signature(nb) for label, secret in SECRETS.items()
                },
            }
        )

    raw = json.loads((HERE / "signing.ipynb").read_text())
    raw["metadata"]["signature"] = "sha256:0000"
    raw["cells"][1]["metadata"]["trusted"] = True
    raw_signature = notary(SECRETS["short"]).compute_signature(nbformat.from_dict(copy.deepcopy(raw)))

    check_cells = []
    for label, case in CHECK_CELLS_CASES.items():
        nb = nbformat.from_dict({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, **copy.deepcopy(case)})
        result = notary(SECRETS["short"]).check_cells(nb)
        check_cells.append({"case": label, "notebook": {"nbformat": 4, "nbformat_minor": 5, "metadata": {}, **case}, "trusted": result, "after": nb})

    out = {
        "secrets": {label: secret.decode("ascii") for label, secret in SECRETS.items()},
        "notebooks": signatures,
        "raw": {"notebook": raw, "secret": "short", "signature": raw_signature},
        "check_cells": check_cells,
    }
    (HERE / "trust.json").write_text(json.dumps(out, indent=1, sort_keys=True, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
