## 2026-11-11, run copies (#658)

`save: "copy"` on `POST /api/notebooks/run` and the host API's
`notebooks.run` keeps the executed notebook at
`runs/<notebook path without .ipynb>/<UTC start>.ipynb`, with `-2`, `-3`
for runs in the same second, and answers its path as `copy`. One
`runs/` folder at the root, not a folder beside each notebook, keeps
notebook folders clean and leaves one place for the run ledger later;
it isn't hidden, because hidden paths aren't readable through the
contents API and the caller couldn't open the copy it was given.

The copy is written through the contents manager, after the run's kernel
is shut down, with the parameters in `metadata.ganymede.params` as
`ganymede execute` records them. A failed run is kept too. If the copy
can't be written, the answer's status is `error` with the reason, after
any run error.

From the host API, a copy needs a read of the source as well as a run
grant. A copy holds the notebook's source and outputs, so without that
check a notebook allowed to run `secret.ipynb` and read `runs/` could
read the secret through its copy, and grants only narrow. A leading dot
in a path part becomes `_` in the copy's folder, so a notebook in a
hidden folder still gets a copy the contents API can write and read.
