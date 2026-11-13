## 2026-11-13, `.ganymede.toml` reads stay under the root (#743)

The after-merge review of #741 found that the folder-default lookup
could read a `.ganymede.toml` outside the server root: through a folder
under the root that is a symlink elsewhere (`symlink_metadata` follows
every component but the last), and on Windows through a backslash or a
drive in `kernelspecs_list`'s `dir`. Only that file's `kernel` or its
parse error could leak, but the docs promised it never reads above the
root, and the contents API refuses those paths.

The lookup now refuses a `dir` with a backslash, a NUL or (Windows) a
colon, as `ApiPath::parse` does, and skips a file whose folder doesn't
canonicalize under the canonical root, as the contents API's
`resolves_under` does. A symlinked folder that stays inside the root
still works.

Smaller fixes from the same review: an existing notebook with no
kernelspec now reports the folder file's mistakes and an uninstalled
kernel, naming the file, instead of ignoring them; `kernelspecs_list`
says when the folder's kernel isn't installed; errors name the file as
the connection sees it; and with `--no-execute` the `notebook_open`
reply says which kernel the folder chose.
