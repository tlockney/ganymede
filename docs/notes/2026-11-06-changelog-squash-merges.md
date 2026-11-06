## 2026-11-06, changelog misses squash merges (#392)

ganymede-ui's 0.4.0 release PR listed 51 of its 75 pull requests: every
squash merge was missing (tlockney/ganymede-ui#326). `cliff.toml` here has
the same preprocessor, `\(#(\d+)\)\s*$`, and without `(?m)` its `$` is the
end of the whole message. GitHub's squash commits carry a body, so none
matched and the catch-all parser dropped them. Ganymede has had one squash
merge since v0.5.0 (#390), which 0.6.0 would have left out.

Matching only the subject would also keep commits on PR branches, which
often end in `(#N)` too. The committer tells them apart: GitHub commits
merges and squashes, and branch commits keep their author's. The parser now
requires `committer.name` to be `GitHub`, and skips "Release vX.Y.Z" as
ganymede-ui does, so the release merge stays out of the next section.
Regenerating the changelog changes released sections only by dropping
their four "Release vX.Y.Z" entries (#36, #124, #147, #219).
