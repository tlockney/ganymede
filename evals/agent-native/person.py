"""A simulated person in a Ganymede notebook room, for the agent-native evals (#824).

Built on the client in e2e/collab/ydoc_client.py (`jupyter_ydoc` and
`pycrdt`, what JupyterLab's collaboration uses), so the server sees a
person's edits as it does in JupyterLab or the apps. Not part of the
product.

    python person.py --server URL --token T --path week.ipynb \\
        --script steps.json --out person.json

The script is a JSON list of steps. Each waits for a condition on the live
document, then acts on it, so what the person does interleaves with what
the agent is doing. A step is

    {"wait": <condition>, "timeout": 120, "do": <action>}

`wait` and `do` are both optional. Conditions (one key each):

    {"agent_cells": N}                at least N cells carry agent provenance
    {"agent_cell_contains": "text"}   a cell with agent provenance holds text
    {"request_done": "id"}            the request cell id is marked done
    {"agent_quiet": S}                no agent cell changed for S seconds,
                                      once there is at least one
    {"sleep": S}                      S seconds from the previous step

Actions:

    {"append": {"cell": T, "text": "..."}}     type text at the end of a cell
    {"replace": {"cell": T, "text": "..."}}    replace a cell's whole source
    {"insert": {"id": "...", "cell_type": "markdown", "source": "...",
                "after": T}}                   add a cell (after T, else at the end)

A target T is a cell id, or {"agent_contains": "text"} for the first cell
with agent provenance holding text.

The client stays connected until it is told to stop (SIGTERM or SIGINT),
then writes `--out`: {"ok", "error", "steps": [{"step", "status",
"waited", "at": ISO UTC time of its action, ...}]}. A step's status is "done", "timeout" (its condition
never held, so nothing after it ran) or "error" ("running" while it waits). Exits 0 once the file is
written, 1 when it could not connect.
"""

import argparse
import asyncio
import json
import signal
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent.parent / "e2e" / "collab"))

from jupyter_ydoc import YNotebook  # noqa: E402
from pycrdt import Doc, create_sync_message  # noqa: E402
from websockets.asyncio.client import connect  # noqa: E402
from ydoc_client import RoomSync, room_ws_url, source  # noqa: E402

CONNECT_TIMEOUT = 30.0
DEFAULT_STEP_TIMEOUT = 120.0
POLL = 0.1


def agent_of(cell):
    a = (cell.get("metadata") or {}).get("ganymede", {})
    a = a.get("agent") if isinstance(a, dict) else None
    return a if isinstance(a, dict) else None


def request_of(cell):
    r = (cell.get("metadata") or {}).get("ganymede", {})
    r = r.get("request") if isinstance(r, dict) else None
    return r if isinstance(r, dict) else None


class Person:
    def __init__(self, ynb):
        self.ynb = ynb
        self.agent_sig = None
        self.agent_changed = time.monotonic()

    def cells(self):
        return self.ynb.get()["cells"]

    def agent_cells(self):
        return [c for c in self.cells() if agent_of(c)]

    def observe(self):
        """Track when an agent cell last changed, for `agent_quiet`."""
        sig = json.dumps([(c.get("id"), source(c)) for c in self.agent_cells()])
        if sig != self.agent_sig:
            self.agent_sig = sig
            self.agent_changed = time.monotonic()

    def holds(self, cond, since):
        (kind, arg), = cond.items()
        if kind == "agent_cells":
            return len(self.agent_cells()) >= arg
        if kind == "agent_cell_contains":
            return any(arg in source(c) for c in self.agent_cells())
        if kind == "request_done":
            for c in self.cells():
                if c.get("id") == arg:
                    r = request_of(c)
                    return bool(r and r.get("done"))
            return False
        if kind == "agent_quiet":
            return bool(self.agent_cells()) and time.monotonic() - self.agent_changed >= arg
        if kind == "sleep":
            return time.monotonic() - since >= arg
        raise ValueError(f"unknown condition {kind!r}")

    def index_of(self, target):
        """The index of the cell a target names; ValueError when none."""
        for i, c in enumerate(self.cells()):
            if isinstance(target, str) and c.get("id") == target:
                return i
            if isinstance(target, dict) and "agent_contains" in target:
                if agent_of(c) and target["agent_contains"] in source(c):
                    return i
        raise ValueError(f"no cell {target!r}")

    def act(self, action):
        (kind, arg), = action.items()
        if kind in ("append", "replace"):
            i = self.index_of(arg["cell"])
            text = self.ynb.ycells[i]["source"]
            with self.ynb.ydoc.transaction():
                if kind == "replace":
                    text.clear()
                text += arg["text"]
            return {"cell": self.cells()[i].get("id")}
        if kind == "insert":
            cell = {
                "cell_type": arg.get("cell_type", "markdown"),
                "id": arg["id"],
                "metadata": {},
                "source": arg["source"],
            }
            if cell["cell_type"] == "code":
                cell.update(outputs=[], execution_count=None)
            if arg.get("after") is None:
                self.ynb.append_cell(cell)
            else:
                self.ynb.ycells.insert(self.index_of(arg["after"]) + 1, self.ynb.create_ycell(cell))
            return {"cell": arg["id"]}
        raise ValueError(f"unknown action {kind!r}")


async def run(args, report):
    flush(report, args.out)
    steps = json.loads(Path(args.script).read_text())
    ws_url = room_ws_url(args.server, args.token, args.path)
    doc = Doc()
    ynb = YNotebook(doc)
    sync = RoomSync(doc)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)

    async with connect(ws_url, max_size=None, proxy=None, open_timeout=CONNECT_TIMEOUT) as ws:
        await ws.send(create_sync_message(doc))

        tasks = [asyncio.create_task(sync.pump(ws)), asyncio.create_task(sync.send(ws))]
        person = Person(ynb)
        report["connected"] = True
        flush(report, args.out)

        async def until(cond, timeout, since):
            deadline = time.monotonic() + timeout
            while not stop.is_set():
                person.observe()
                if person.holds(cond, since):
                    return True
                if any(t.done() for t in tasks):
                    raise RuntimeError(f"websocket closed ({ws.close_code} {ws.close_reason!r})")
                if time.monotonic() > deadline:
                    return False
                await asyncio.sleep(POLL)
            return False

        # Synced: the room's cells have arrived.
        deadline = time.monotonic() + CONNECT_TIMEOUT
        while len(ynb.ycells) == 0 and time.monotonic() < deadline:
            await asyncio.sleep(POLL)

        since = time.monotonic()
        for n, step in enumerate(steps):
            entry = {"step": n, "status": "running"}
            report["steps"].append(entry)
            started = time.monotonic()
            try:
                flush(report, args.out)
                cond = step.get("wait")
                if cond is not None:
                    ok = await until(cond, step.get("timeout", DEFAULT_STEP_TIMEOUT), since)
                    entry["waited"] = round(time.monotonic() - started, 1)
                    entry["wait"] = cond
                    if not ok:
                        entry["status"] = "timeout"
                        break
                if step.get("do") is not None:
                    entry["do"] = step["do"]
                    entry.update(person.act(step["do"]))
                    entry["at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
            except Exception as e:  # a bad target or script: the run can't mean anything
                entry["status"] = "error"
                entry["error"] = repr(e)
                break
            entry["status"] = "done"
            flush(report, args.out)
            since = time.monotonic()
        report["ok"] = all(s["status"] == "done" for s in report["steps"])
        flush(report, args.out)

        # Stay in the room, so the server keeps it open and saves, until told.
        await stop.wait()
        # Let the last edits reach the server.
        while not sync.outgoing.empty():
            await asyncio.sleep(POLL)
        await asyncio.sleep(0.5)
        for t in tasks:
            t.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        report["final"] = [(c.get("id"), source(c)) for c in person.cells()]


def flush(report, out):
    Path(out).write_text(json.dumps(report, indent=1) + "\n")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--server", required=True)
    p.add_argument("--token", required=True)
    p.add_argument("--path", required=True, help="notebook path under the server root")
    p.add_argument("--script", required=True, help="JSON file: the steps")
    p.add_argument("--out", required=True, help="JSON file to write")
    args = p.parse_args()
    report = {"ok": False, "connected": False, "error": None, "steps": []}
    code = 0
    try:
        asyncio.run(run(args, report))
    except Exception as e:
        report["error"] = repr(e)
        code = 0 if report["connected"] else 1
    flush(report, args.out)
    return code


if __name__ == "__main__":
    sys.exit(main())
