## 2026-11-17, design note: several roots in one contents tree (#845)

ganymede-ui#465 wants the iPhone and iPad sidebar to list the app's own
folder beside each folder the person picked, but the app's server is
rooted at one folder (`AppModel.serverRoot`), and opening another folder
restarts it. The note, [Several folders in one server](../multi-root.md),
compares named mounts under one contents tree in the server with one
server per folder in the app, covering API paths, grants and their
stamps, rooms and watching, moves between folders, security, and what
the apps need.

It recommends mounts: each mount is a contents root as today, with its
own `FsContents` and its own `Collab` (file ids, rooms, the move lock,
path locks and stamps), behind a router that takes the first path
segment as the mount's name; only the file watcher is shared. Mounts
may not overlap: a folder at, inside or above another mount's would
give one file two ids and two rooms, and the embedded server has no
room locks to stop them saving over each other. A move between mounts
is refused, since the two are separate roots with their own security
scopes, ids and grants, not because of volumes. File ids are keyed by
the mount's real folder, so a mount's grants are the same rows a server
or `ganymede grant` on that folder uses, and the cross-root work of #827
and #831 covers the other mounts with no change. One server per folder
needs no core change but makes the page handle several servers, origins
and tokens wherever it touches a path. Rooting the iOS server at `/`, as
the Mac app does, was set aside: it would serve the whole app container
and put the container's path in every API path. Six questions are left
for Thomas, among them mount names, adding a folder without a restart,
and whether the default `read = [""]` should reach every mount.
