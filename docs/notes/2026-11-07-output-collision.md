## 2026-11-07, two notebooks to one output file (#511)

`ganymede execute a/x.ipynb b/x.ipynb -o '{notebook_name}-out.ipynb'`
saved both results to `x-out.ipynb`, the second overwriting the first.
`{notebook_name}` is the file stem, and the output path doesn't follow
the input's directory, as in `jupyter execute`. Execute now resolves
every output before it runs anything and refuses two inputs that land
on one file, naming both. Found in the 0.7.0 code review (#440).
