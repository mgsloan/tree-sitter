# Bugfix fork conventions

## Source and generated branches

- `mgsloan-bugfix-maintenance` owns `tools/bugfixes/`, the root `README.md`, and
  this file. Its usual worktree is `~/oss/tree-sitter-bugfixes`.
- `mgsloan-bugfixes` is generated from the pinned upstream tag and the source
  branches in `tools/bugfixes/manifest.json`. Put durable changes on their source
  branches; edits committed only to the aggregate are lost during regeneration.
- Keep `CLAUDE.md` as a relative symlink to `AGENTS.md`, so both tools read the
  same conventions.

## Fix branches

- Use an independent `fix/<descriptive-kebab-case-name>` branch per bug. Preserve
  the existing legacy name `fix-previous-sibling-index-underflow`.
- Start from an appropriate upstream commit, or extract only the relevant
  commits onto a clean branch. Keep unrelated fixes, maintenance changes, and
  tree-squatter history outside the patch series. The updater rejects merges
  within a source range.
- Add a concise regression that fails before the fix and passes afterward.
  Include controls when needed to protect adjacent valid behavior. Run the
  relevant tests; documentation-only patches may have no regression tests.
- Preserve published reproduction branches. Use a separate fix branch rather
  than replacing the reproduction with an implementation.
- Do not rewrite original fix branches as part of an aggregate rebuild. The
  updater rebases temporary copies under `bugfixes/rebased/`.

## Manifest updates

- Edit and commit `tools/bugfixes/manifest.json` on `mgsloan-bugfix-maintenance`
  before rebuilding. Add a unique kebab-case `id`, resolvable `ref`, exact `base`
  commit, `issue` URL (or `null` for an unfiled issue), regression test names, and
  notes explaining the fix and relevant limitations.
- `base` must be an ancestor of `ref` and exclude all unrelated commits. Default
  permitted paths are `lib/` and `crates/cli/src/tests/`; add explicit
  `allowed_paths` only when the patch needs other files.
- Keep the maintenance entry's base fixed unless intentionally rebasing that
  source branch. Its allowed paths include the maintenance tooling and root
  documentation files.
- Keep `upstream` pinned to `refs/tags/v0.27.0` unless asked to change it.
- When a fix lands upstream, verify the regression against the selected upstream
  base before disabling the entry. Record the upstream commit in `notes`; a
  closed issue alone is insufficient, especially with a pinned older base.
- Preserve disabled entries and their rationale. The #5935 performance
  optimization remains disabled.

## README updates

- Update the root `README.md` on the maintenance branch whenever an enabled fix,
  issue URL, source ref, classification, or upstream base changes. Preserve the
  upstream Tree-sitter introduction below the fork-specific sections.
- List every enabled manifest entry exactly once in the three fix sections;
  omit disabled entries. Include the source branch, a short behavioral
  description, and an issue link for reported issues. Mark documentation-only
  patches as such.
- Keep existing clean fixes in their current section. New agent-authored fixes
  belong in a vibecoded section unless the user identifies them as cleaned up.
  Passing tests alone does not change this classification. Use the manifest's
  `issue` URL versus `null` to distinguish reported and unreported fixes.
- Keep the patch table and fix count in `tools/bugfixes/README.md` consistent with
  the manifest and root README.

## Regenerating the aggregate

- Follow `tools/bugfixes/README.md`. Ensure source changes are committed and
  switch any checkout of `mgsloan-bugfixes` to another branch or detached HEAD
  before running `python3 tools/bugfixes/update.py` from the maintenance worktree.
  Preserve uncommitted user changes when switching.
- Supply `--fixtures` if prepared grammars live in another checkout. Use
  `--no-fetch` when deliberately building from the current local refs.
- Let the updater run all manifest checks before it updates the aggregate.
  Resolve conflicts in its printed worktree, stage resolutions, and resume with
  `--continue`. Fix implementation defects on the original source branch and
  start a fresh build.
- Inspect the resulting aggregate and source refs. Return the original checkout
  to `mgsloan-bugfixes` after a successful rebuild. The updater does not push;
  publish only when requested, using `--force-with-lease` for the aggregate.
