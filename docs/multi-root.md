# Several folders in one server

A design note for [#845](https://github.com/tlockney/ganymede/issues/845),
asked for by
[ganymede-ui#465](https://github.com/tlockney/ganymede-ui/issues/465): on
the iPhone and iPad the sidebar should list the app's own folder (On My
iPhone › Ganymede, where the examples go) and each folder the person
picked, each as its own section, with notebooks in every one opening,
saving and running as they do today. It compares two ways to get there,
recommends one, and leaves some questions for Thomas. Nothing here is
built yet.

## What happens today

As of 2026-11-17.

- **One root per server.** `ganymede serve`, and the server the apps
  start through ganymede-ffi, serve one directory, `root_dir`. Every API
  path is relative to it (`FsContents`), and a path that leaves it, by
  `..`, an absolute path or a symlink, is refused. The same root is
  handed to the file ids (`FileIds::open(db, root)`), to document rooms
  and their watcher (`Collab::new(ids, Some(root))`), to grant stamping
  (`Stamps`), to kernels' working directories (`kernel_cwd`), to
  per-folder settings (`.ganymede.toml`, read up to the root), to the
  host API's grant checks, and to MCP's paths and publish redactions.
- **The Mac app** roots its server at `/` and opens every window by
  absolute path, so any folder is reachable without a restart.
- **The iPhone and iPad apps** root it at the one open folder
  (`AppModel.serverRoot`): the app's Documents folder, or a folder from
  the picker, held by a security-scoped bookmark. Opening another folder
  restarts the server on it, which shuts down its kernels; a
  `ganymede://open` link to a notebook in another granted folder does the
  same, and is refused while a text file is open, since it would save
  into the new folder. The app turns API paths into file paths by joining
  them to that folder (`serverRootFolder`, `AppKernels.notebookRoot`).
- **File ids and grants are per (root, path)** in one database that
  every server and `ganymede grant` share (`ganymede_file_ids.db`). A
  grant carries a stamp of its file (#827). A save through any server
  checks and restamps the grants every other root holds on the same file
  (`held_with`), and a rename or delete moves or drops the ids of every
  other root above or inside the path (#831). Saves, renames and creates
  wait only for each other's paths (#830, #833).
- **Rooms** are named by file id
  (`json:notebook:<id>`), watch the file's directory on Linux, macOS and
  Windows, and poll elsewhere, which includes iOS. A room's lock, which
  keeps a second server on the same machine read-only, is off in the
  apps' embedded server.

## The two options

**(a) Named mounts in the server.** The server takes a list of folders,
each with a name, instead of one root. The name is the first segment of
every API path in that folder: `Ganymede/Examples/Orbits.ipynb`,
`Research/notes.ipynb`. The root, `""`, is a directory that lists the
mounts and holds nothing else.

**(b) One server per folder in the app.** The app starts a server for
each folder, each exactly as today, and tells the page which server each
section of the sidebar, and so each path, belongs to.

### Paths and Jupyter compatibility

(a) keeps one contents tree, so it stays one `jupyter_server`-shaped
server: JupyterLab, VS Code and nb-cli see a root with a folder per
mount and need nothing new. The root is read-only: listing it works,
and creating, uploading, renaming or deleting at the top level is
refused with 403, as a read-only root is today. A mount can't be renamed
or deleted through the API, as the root can't be now. Without `mounts`
in the config the server is exactly what it is today, with no prefix.

Mount names are path segments, so they follow the API path rules: no
`/`, `\`, NUL or leading `.`, and on Windows no `:`; the server refuses
duplicates. They should be stable, since they are in every path a client
keeps (open tabs, recent files, sessions, links); the app chooses
them and keeps each with its bookmark.

(b) changes nothing in the API, but the page holds one backend per
server. ganymede-ui's page has one `ServiceManager` (`backends.ts`); it
would need one per folder, a contents listing merged from all of them,
kernel and session lists merged too, and every path the page passes
around (tabs, links, the device permission relay, Shortcuts) paired with
its server. Each server has its own port and so its own origin, and the
page keeps its saved state in local storage per origin. A page on one
origin calling the others needs CORS and their tokens.

### Grants and their stamps

(a): each mount's file ids are opened with the mount's real folder as
the root, `FileIds::open(db, folder)`, as a server on that folder alone
would. So a mount's grants are the same rows a `ganymede serve` or
`ganymede grant` on that folder reads and writes, and the cross-root
work of #827 and #831 (checking and restamping other roots' grants on a
save, moving and dropping them on a rename or delete) applies to each
mount unchanged, with the other mounts as other roots. Stamps and the
path locks of #830 and #833 stay per mount, so a save in one mount never
waits on a rename in another.

A per-notebook grant's paths are relative to its own mount, since the
grant is shared with servers rooted at that folder. The host API adds
the mount's name before matching. A notebook's grant therefore can't
name a file in another mount, and a symlink from one mount into another
is refused like any path outside the root. The config's `[host]`
defaults are the server's own, so they apply to the whole tree: the
default `read = [""]` lets a notebook read every mount (see the open
questions).

(b): every server is today's, so grants and stamps need nothing new.
A notebook can never read or run anything in another folder.

### Rooms and file watching

(a): file ids are UUIDs, unique across roots in the database, so room
names stay `format:type:fileId` and a room is found by asking each
mount's ids for the id. A room opened through
`PUT /api/collaboration/session/{path}` goes to the mount its path names.
The watcher already watches absolute directories, one OS watcher per
server, so it serves every mount; on iOS rooms poll, per room, as now.
Room locks are keyed by absolute path and work per mount where they are
on.

(b): rooms are per server, unchanged. The page connects each notebook's
room to its own server.

### Rename and move across roots

Within a mount, everything is as today. A rename from one mount to
another is refused with 400 ("can't move between folders"). Today a
rename is one `rename(2)`, which can't cross volumes, and two picked
folders are usually on different ones (the app's container, iCloud
Drive, a file provider). A copy across mounts can work: the router
makes it a read from one mount and a save to the other. The page can offer Move as a copy then
a delete. The copy is a new file, so it has no grant, as any copy today.

In (b) there is no API call that spans two servers; the page would do
the same copy and delete, as two servers' calls.

### Security

(a): each mount is its own `FsContents`, rooted at the canonical folder
the person picked, with today's checks (`ApiPath::parse`, symlinks
resolved and kept inside). Nothing outside the picked folders and the
app's folder is reachable, unlike rooting at `/` on iOS (below). Mount
names come from the app's config, never from a client. Each picked
folder is readable only while the app holds its security scope, which it
must hold for as long as the server serves it, as it does for the one
folder now.

(b): the same per folder, plus one token and one listener per server.

### What the apps need

For (a), in ganymede-ui:

- Start the server with a `mounts` list: the app's folder and each
  picked folder, by name and path, holding every picked folder's scope.
  The Mac app keeps rooting at `/`.
- Replace joining an API path to one folder with a lookup by the first
  segment: `serverRootFolder`, `NotebookActions.apiPath`,
  `AppKernels.notebookRoot`, `KernelHost`'s paths, the Shortcuts
  intents, Spotlight and `ganymede://open` links.
- In the page, draw each top-level entry of the root listing as a
  section, and offer no new file or folder at the top level.
- Keep picked folders as bookmarks across launches, and remove one by
  restarting the server without it, after closing its notebooks.
- Add the examples to the app's mount, as now.

For (b): a server per folder, each with its own port, token and
registration of the in-app TypeScript kernel (`AppKernels.serve`), which
today registers with one server; a backend per server in the page,
merged listings, and a server beside every path the page and the native
side pass to each other.

## Recommendation

**(a), named mounts.** It keeps one server, one origin, one kernel list
and one in-app kernel registration, so the page and the native side
change only where they turn API paths into files. Each mount reuses what
a single-root server already does, grants included: the work is a
router in front of a set of today's roots, not a change to any of them.
It also ends the restart when a link names a notebook in another
granted folder, and with it the refusal while a text file is open.
(b) needs no core change, but moves the multi-root problem into the
page, which then has to know about several servers wherever it handles a
path, and costs a runtime, a listener and a port per folder on the
phone.

In the server, a mount is a contents root as today, with its own
`FsContents`, file ids, stamping and path locks. `AppState` holds the
mount table; `contents` becomes a `Contents` that strips the first
segment and calls the mount's, adding it back to the paths it returns,
and the other users of the root (rooms, the host API, kernels'
working directories, `.ganymede.toml`, MCP) look the mount up by path.

A third option was considered: root the iOS server at `/`, as the Mac
app does. It needs no new code in the server, but it serves the whole
app container, the cookie secret and the file id database included; and
its API paths carry the container's path, which can change between
installs and restores, so every path a client kept would break.

## Open questions for Thomas

1. **Mount names.** The folder's name as Files shows it, with " 2" added
   when two match, kept with the bookmark (recommended)? Or a fixed
   short id, with the sidebar showing the folder's name, which survives a
   folder being renamed in Files but puts ids in paths?
2. **Adding and removing a folder.** Restart the server with the new
   list, which shuts down every kernel, as opening a folder does now
   (recommended for the first version)? Or a call that adds and removes
   a mount on the running server?
3. **Default reads across mounts.** With `read = [""]`, a notebook in
   one folder may read every other folder the person picked. Keep that,
   since the person picked them all (recommended)? Or make `""` in the
   defaults mean the notebook's own mount?
4. **Move between folders.** Refuse it in the server and offer copy
   then delete in the page, with no grant carried across (recommended)?
   Or move it in the server, keeping the grant?
5. **Link Folders and sidebar folders.** Should the folders granted for
   `ganymede://open` links (Settings › Link Folders) be the same list as
   the sidebar's picked folders, so every granted folder is mounted?
6. **Outside the apps.** Offer `mounts` in `ganymede.toml` and as a
   `ganymede serve` flag too, or only in the apps' config at first? A
   server with mounts has no single `root_dir` to write in its server
   info file, which `ganymede mcp` and other clients read to map local
   paths to API paths.
