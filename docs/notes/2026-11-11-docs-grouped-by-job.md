## 2026-11-11, docs sidebar grouped by job (#689)

The docs sidebar grouped pages by reading level and appended
ganymede-ui's pages as a group of their own, so the apps' user guide was
the last thing in the sidebar and sat beside that repository's release
steps. The groups now follow what the reader is doing: Get started, Use
the apps, Work with an agent, Use Jupyter tools, Reference, Cell APIs,
How it works, Background, Contributing. No page's text or URL changed.

ganymede-ui's pages are placed by name from `uiPlaced` in levels.ts:
apps, shortcuts and device under Use the apps; architecture, deploying
and releasing under Contributing. `groups()` takes which of them the
fetch copied, so a build without `npm run fetch-ui` still has no dead
entries, and a page ganymede-ui adds later lands in a "ganymede-ui"
group until levels.ts places it. Security and Privacy moved from the
guides to How it works, rooms to the agents group, Other kernels to
Use Jupyter tools. The top nav is Home, Get started, Apps, Agents,
Reference, Roadmap.

First step of #689, the restructuring from the docs and web presence
review; the new guides, examples and reference pages come next, each
with its own group entry when its page exists.
