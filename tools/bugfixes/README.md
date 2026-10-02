# Maintaining mgsloan-bugfixes

`manifest.json` lists nine bugfix branches, their issue links, regression tests,
and the exact base commit excluded from each patch series. Source branches remain
unchanged. `mgsloan-bugfix-maintenance` owns this tooling and manifest; the generated
`mgsloan-bugfixes` branch includes them too.

From the maintenance worktree:

```sh
cd ~/oss/tree-sitter-bugfixes
python3 tools/bugfixes/update.py
```

The updater fetches `origin` and `mgsloan`, snapshots all selected source refs,
rebases temporary `bugfixes/rebased/*` branches onto `origin/master`, and merges
them into `bugfixes/integration-candidate` with one merge per fix. It runs its
own updater tests and the native Tree-sitter CLI library suite before updating
`mgsloan-bugfixes`. It also keeps the previous tip at `refs/bugfixes/previous`
and records the source, upstream, and result commits in
`<git-common-dir>/bugfixes/last-build.json`.

All temporary Git operations happen in `<git-common-dir>/bugfixes/worktree`.
Your current checkout and original fix branches are preserved. The target branch
must not be checked out in any worktree when running an update. This workflow
requires Git, Python 3, Rust/C build tools, and the normal grammar-fixture setup
used by Tree-sitter's tests. Python uses `fcntl` for the build lock (Unix).

Existing grammar fixtures are shared via symlinks. If they are elsewhere, pass
`--fixtures /path/to/test/fixtures/grammars`. On a fresh clone, prepare fixtures
with `cargo xtask fetch-fixtures` and `cargo xtask generate-fixtures` first. The
Cargo target directory defaults to the invoking checkout's `target`; set
`CARGO_TARGET_DIR` to share another build cache. `--no-fetch` uses current refs.

## Conflicts and failed checks

Repository-local `rerere.enabled=true` records and reuses conflict resolutions.
The script leaves conflicts available for inspection and keeps the previous
integration branch intact. Resolve the conflicted files in the printed build
worktree, stage them with `git add`, and run:

```sh
python3 tools/bugfixes/update.py --continue
```

The script continues the pending rebase or merge and resumes the remaining
steps. You may also complete the Git operation manually before `--continue`.
Reused resolutions still need staging; `rerere.autoupdate` is deliberately not
enabled. A failed check can be retried with `--continue` after fixing its cause.
If a code change is needed for a failed check, put it on the original fix branch
and start a new build instead of changing only the generated branch.

To abandon a build, abort any pending Git operation in its worktree, preserve
any useful changes, make that worktree clean, and remove only
`<git-common-dir>/bugfixes/state.json`. Then start a new build. The snapshot in
a stopped build is fixed; source edits and new upstream commits are picked up
only by a new build.

## Adding, changing, and retiring fixes

Develop each fix on an independent branch. Add its local or remote ref, its base
commit, issue URL, and regression names to `manifest.json`, then commit the
manifest on `mgsloan-bugfix-maintenance` before rebuilding. `base` must be an
ancestor of `ref` and must exclude unrelated history. Merge-containing source
ranges and changes outside the entry's allowed paths are rejected. The default
allowed paths are `lib/` and `crates/cli/src/tests/`.

Keep the maintenance base fixed unless intentionally rebasing that source branch;
update the manifest's base along with such a rebase. The updater rebases only its
temporary copies, so routine upstream updates need no source-branch rewrites.

When a fix lands upstream, verify that its regression passes there, then set
`enabled` to `false` with a note about the upstream commit. Git can drop patches
that become empty while rebasing, but an issue's closed state alone does not
establish that the fix landed. The optional #5935 optimization is disabled: it
is a performance change that overlaps the #5934 fix.

## Current patches

| Issue | Source branch | Status |
| --- | --- | --- |
| #5932 | `fix/disable-wildcard-pattern` | Existing fix, also in tree-squatter main |
| #5934 | `fix/hidden-zero-width-descendant` | Existing fix, also in tree-squatter main; closed upstream without landing |
| #5950 | `fix-previous-sibling-index-underflow` | Existing fix, also in tree-squatter main |
| #5987 | `fix/first-named-child-for-byte-upstream` | Extracted only commit `5f4109768` from a branch containing tree-squatter history |
| #5948 | `fix/visible-alias-field-lookup` | New fix; preserves the published reproduction branch |
| #5949 | `fix/child-with-descendant-self` | New fix; preserves the published reproduction branch |
| Unfiled | `fix/previous-sibling-alias-after-extra` | New fix for the confirmed local cursor reproduction |
| Unfiled | `fix/non-rooted-query-range` | Rejects non-rooted queries outside the cursor root's byte or point range; preserves sibling matches spanning a range |
| Unfiled | `fix/non-rooted-wildcard-range-pruning` | Preserves wildcard sibling matches when hidden repetitions are pruned by intersecting and containing ranges |

The query capture-prefix and hidden-subtree reuse observations in the local
`potential-upstream-bugs.md` have no filed issues or settled fixes and are not
included. The old crates.io version report #610 is unrelated to runtime fixes.

Nothing is pushed by the updater. Publishing a rebuilt aggregate requires a
force push because its merge history is regenerated; use `--force-with-lease`
when you choose to publish it. Share the maintenance and source branches too
if you want another clone to reproduce the build.
