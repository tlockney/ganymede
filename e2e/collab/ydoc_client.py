"""A jupyter-collaboration client against a Ganymede document room.

Test oracle only (#258): `jupyter_ydoc` and `pycrdt` are what
`jupyter-collaboration` and JupyterLab's Python side use, so a room that
syncs with them speaks the same schema and protocol. Run by
crates/ganymede-cli/tests/collab.rs; not part of the product.

    python ydoc_client.py <server url> <token> <root dir> <notebook path>

The notebook must hold two code cells with ids "a" and "b". The client
syncs, checks it reads what the file holds, then edits cell "a" and adds a
cell while another program changes cell "b" on disk. Both sides must end
with all three changes, the file as the room saved it. Exits non-zero
with a message on any mismatch.
"""

import asyncio
import json
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

from jupyter_ydoc import YNotebook
from pycrdt import (
    Doc,
    YMessageType,
    create_sync_message,
    create_update_message,
    handle_sync_message,
)
from websockets.asyncio.client import connect

TIMEOUT = 20.0

# Loopback calls skip any `http_proxy` in the environment.
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def source(cell):
    s = cell.get("source", "")
    return "".join(s) if isinstance(s, list) else str(s)


def file_sources(path):
    nb = json.loads(Path(path).read_text())
    return [(c.get("id"), source(c)) for c in nb["cells"]]


def doc_sources(ynb):
    return [(c.get("id"), source(c)) for c in ynb.get()["cells"]]


async def main(server, token, root, nb_path):
    file = Path(root) / nb_path
    req = urllib.request.Request(
        f"{server}/api/collaboration/session/{urllib.parse.quote(nb_path)}",
        data=json.dumps({"format": "json", "type": "notebook"}).encode(),
        method="PUT",
        headers={"Authorization": f"token {token}", "Content-Type": "application/json"},
    )
    with OPENER.open(req, timeout=TIMEOUT) as r:
        session = json.load(r)
    ws_url = (
        server.replace("http", "ws", 1)
        + f"/api/collaboration/room/json:notebook:{session['fileId']}"
        + f"?sessionId={session['sessionId']}&token={token}"
    )

    doc = Doc()
    ynb = YNotebook(doc)
    applying = False
    outgoing = asyncio.Queue()

    def on_update(event):
        if not applying:
            outgoing.put_nowait(create_update_message(event.update))

    doc.observe(on_update)

    async with connect(ws_url, max_size=None, proxy=None, open_timeout=TIMEOUT) as ws:
        await ws.send(create_sync_message(doc))

        async def pump():
            nonlocal applying
            async for msg in ws:
                if not isinstance(msg, bytes) or msg[0] != YMessageType.SYNC:
                    continue
                applying = True
                try:
                    reply = handle_sync_message(msg[1:], doc)
                finally:
                    applying = False
                if reply is not None:
                    await ws.send(reply)

        async def send():
            while True:
                await ws.send(await outgoing.get())

        tasks = [
            asyncio.create_task(pump(), name="pump"),
            asyncio.create_task(send(), name="send"),
        ]

        def died():
            """Why the pump or send task stopped, if one has."""
            for t in tasks:
                if not t.done():
                    continue
                if not t.cancelled() and t.exception() is not None:
                    return f"{t.get_name()} failed: {t.exception()!r}"
                return (
                    f"{t.get_name()} stopped: websocket closed "
                    f"({ws.close_code} {ws.close_reason!r})"
                )
            return None

        async def until(check, what):
            deadline = time.monotonic() + TIMEOUT
            while not check():
                why = died()
                if why is not None:
                    fail(f"{why}, waiting for {what}")
                if time.monotonic() > deadline:
                    fail(f"timed out waiting for {what}")
                await asyncio.sleep(0.05)

        # Synced: the client reads what the file holds.
        await until(lambda: len(ynb.ycells) == 2, "the initial sync")
        if doc_sources(ynb) != file_sources(file):
            fail(f"synced {doc_sources(ynb)}, file has {file_sources(file)}")
        if ynb.get()["nbformat"] != 4:
            fail(f"bad meta: {ynb.get()}")

        # Edit and add a cell here, and change cell b on disk, at once.
        ycell_a = ynb.ycells[0]
        text = ycell_a["source"]
        text += "  # from python"
        ynb.append_cell(
            {"cell_type": "markdown", "id": "py", "metadata": {}, "source": "added by python"}
        )
        nb = json.loads(file.read_text())
        nb["cells"][1]["source"] = source(nb["cells"][1]) + "  # from disk"
        file.write_text(json.dumps(nb, indent=1) + "\n")

        def converged():
            try:
                on_disk = file_sources(file)
            except (OSError, ValueError):
                return False  # mid-write
            mine = doc_sources(ynb)
            return (
                mine == on_disk
                and len(mine) == 3
                and mine[0][1].endswith("  # from python")
                and mine[1][1].endswith("  # from disk")
                and mine[2] == ("py", "added by python")
            )

        await until(converged, "both sides to hold every change")
        for t in tasks:
            t.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
    print(json.dumps({"ok": True, "cells": doc_sources(ynb)}))


if __name__ == "__main__":
    if len(sys.argv) != 5:
        fail(__doc__)
    asyncio.run(main(*sys.argv[1:]))
