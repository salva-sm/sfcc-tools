# Contributing

Contributions are welcome. This repository has one maintainer, [@salva-sm](https://github.com/salva-sm),
who reviews and approves every change; the rules below exist so that stays manageable.

## Before you write code

**Open an issue first** for anything beyond a typo or an obvious one-line fix: a new
feature, a new crate, a change to a CLI flag, a file format or what one tool writes for
another. Say what you want to change and why. Wait for a reply before starting — a PR
for something that was not agreed may be closed, however good the code.

Small fixes (typos, a clear bug with a test that shows it) can go straight to a PR.

## Workflow

1. **Collaborators** push their branches to this repository. **Everyone else forks** it.
   Either way, nothing reaches `main` except through a PR: `main` is protected, and a
   PR needs CI to pass and the maintainer's approval before it can be merged.
2. **Branch from an up-to-date `main`**, one branch per change:
   `feature/<short-name>`, `fix/<short-name>` or `docs/<short-name>`.
3. **Keep the PR small and about one thing.** One crate where possible. No drive-by
   refactors, renames or reformatting of code you did not otherwise touch — send those
   separately.
4. **Run the checks locally** (below) before pushing.
5. **Open the PR against `main`**, as a draft if it is not ready for review. Fill in the
   template, link the issue (`Closes #12`).
6. **Keep it current by rebasing** on `main` (`git pull --rebase origin main`, or `upstream main`
   from a fork), not by
   merging `main` into your branch.
7. After review, push fixes as new commits so the review can see what changed. Do not
   force-push while a review is in progress unless asked.

PRs are merged with **squash**, so your branch's commit history does not need to be
tidy; the PR title becomes the commit message. Pushing after the approval dismisses it,
and the PR needs approving again.

## What the maintainer does, not you

These are kept out of PRs because they conflict between branches and decide what users
get:

- **Versions.** Do not change `version` in any `Cargo.toml`. If a change needs a bump,
  the review says which one, and it goes in as the last commit just before merging.
- **Releases and tags.** Never push a `v*` tag; that triggers the release workflow.
- **CI and release workflows** (`.github/`), `Cargo.toml` at the workspace root, and
  `CONTRIBUTING.md`/`CODEOWNERS`, unless the issue was about exactly that.
- **New dependencies.** Allowed, but say in the PR why the dependency is needed and
  why the standard library or an existing dependency does not cover it.

## Checks

CI runs these on every PR and must pass before a merge. Run them first:

```bash
cargo fmt --all --check
cargo clippy -p <crate> --all-features --all-targets -- -D warnings
cargo test -p <crate>
```

`sfcc-core` is tested with `--all-features` (the WebDAV tests need it). If you touch
`grammar/`, regenerate the parser (`npx tree-sitter generate`) and commit `src/` with it;
CI fails on a stale one. If you touch `extensions/isml`, check it still builds for
`wasm32-wasip2`.

A change in `sfcc-core` affects every tool that uses it: run the tests of the crates that
depend on what you changed, not only the core's.

## Standards

- **Tests.** A bug fix comes with a test that fails without it. New behaviour comes with
  tests for it.
- **No secrets, no instance data.** Never commit a `dw.json`, credentials, client ids,
  hostnames of a real instance, or logs and orders from one. Use made-up values in tests
  and examples.
- **Code reads like the code around it**: same naming, error handling and comment
  density as the rest of the crate. Comments say *why*, not what.
- **Docs.** If a change alters what a user sees — a flag, an output, a file — update that
  crate's README in the same PR.
- **Cross-platform.** The tools run on Windows, macOS and Linux. Paths, line endings and
  process handling must work on all three.
- **AI-assisted code is fine**, as long as you have read, understood and tested every
  line you submit. You answer for it in review.

## PR title and description

The title becomes the commit on `main`. Use the repository's style: the crate, a colon,
and what is now true, in plain words.

```
log-diff: an order number is not the failure
sfcc-upload: ensure-active puts our code version back
```

The description says what changed, why, and how you tested it.

## Review

- Expect questions and change requests; they are about the code, not you.
- A PR with no activity from its author for 30 days may be closed. It can be reopened.
- The maintainer may decline a change that works but does not fit where the project is
  going. Opening an issue first is how you avoid that.

## Licence

By contributing you agree that your contribution is licensed under the
[MIT licence](LICENSE), like the rest of the repository.
