---
name: release
description: Cut a ZedTerm release end to end — bump the terminal_app version, commit, tag vX.Y.Z, push so GitHub Actions builds the deb and both macOS DMGs and publishes the GitHub release, then verify the artifacts.
whenToUse: Use when the user asks to release, publish, cut, or ship a new ZedTerm version.
---

# ZedTerm release

One version number drives everything: the `[package] version` in
`crates/terminal_app/Cargo.toml`. The tag, the deb, and both DMGs all derive
from it, and CI refuses to publish if any of them disagree. Do not bump
versions anywhere else — nothing else declares one.

## Release contract

| Thing | Value |
|---|---|
| Version source | `crates/terminal_app/Cargo.toml`, `[package] version` |
| Commit message | `release: vX.Y.Z` |
| Tag | annotated, `vX.Y.Z`, same message |
| Commit contents | only `crates/terminal_app/Cargo.toml` + `Cargo.lock` |
| Push remote | `origin` (`github.com:ruanimal/zed-term.git`) — this is where Actions run |
| Workflow | `.github/workflows/release.yml`, triggered by pushing a `v*` tag |
| Assets | 2 debs (`zedterm-X.Y.Z-linux-amd64.deb`, `zedterm-X.Y.Z-linux-arm64.deb`) + 2 DMGs (`macos-x86_64`, `macos-arm64`), exactly 4 |

## Steps

### 1. Pre-flight

```bash
git status --short          # release commit must be the only thing you add
git branch --show-current   # expect zed-term, tracking origin/zed-term
git tag -l 'v*' | sort -V | tail -5
```

Land unrelated work first: the release commit carries the version bump and
nothing else, so a dirty tree means commit or stash before starting.

Run the normal checks from the README on the code being released:

```bash
cargo test -p terminal_app --lib
cargo test -p terminal_core --lib
cargo fmt --all -- --check
cargo clippy -p terminal_app --all-targets -- --deny warnings
git diff --check
```

### 2. Pick the version

Compare `git tag -l 'v*' | sort -V | tail -1` with the current Cargo version —
they should match. The new version must be strictly greater. This project is
pre-1.0, so patch for fixes, minor for features.

### 3. Bump and commit

Edit the single `version = "..."` line under `[package]` in
`crates/terminal_app/Cargo.toml`, then refresh the lockfile. `Cargo.lock`
records the member version, and CI builds with `--locked`, so a stale lock
fails the build rather than silently resolving:

```bash
cargo check -p terminal_app
git diff --stat            # expect exactly Cargo.toml + Cargo.lock
```

Confirm the two agree, then commit:

```bash
git add crates/terminal_app/Cargo.toml Cargo.lock
git commit -m 'release: vX.Y.Z'
```

### 4. Tag and push

```bash
git tag -a vX.Y.Z -m 'release: vX.Y.Z'
git push origin zed-term
git push origin vX.Y.Z     # this push is what starts the release workflow
```

`origin` is the GitHub repo that runs Actions. The `my` remote is a separate
mirror and does not build or publish anything — never push the tag there
expecting a release.

### 5. Confirm the run started, then hand off

Do not wait for the build. The Linux job has a 45-minute timeout and each
macOS job 60, and a full run routinely takes tens of minutes — blocking on it
wastes the session. Confirm the tag push started a run, report the run URL,
and stop:

```bash
gh run list --workflow release.yml --limit 3   # the new run should be in_progress
gh run view <run-id>                           # after a few seconds: check the prepare job
```

`prepare` finishes in seconds even though the build jobs run for much longer,
so this one early check is worth doing — it catches a tag/version mismatch
immediately instead of after an hour of building. The remaining jobs are
`linux` (amd64 + arm64 matrix), `macos` (x86_64 + arm64 matrix), and `publish`, which downloads the
`zedterm-*` artifacts, hard-fails unless it finds exactly 4 files, and creates
the GitHub release with generated notes and `draft: false`.

Verification is a separate, later step — run it when the user asks or the run
is known to be finished, not in a wait loop:

```bash
gh run list --workflow release.yml --limit 1
gh release view vX.Y.Z
```

Success is a published release with all four assets attached. A run that is
merely still going is not a failure: report that it is in progress and the
release URL, and only report failure once a job actually fails.

## Traps

- **Tag/version mismatch aborts the run.** `prepare` compares the tag against
  `terminal_app`'s Cargo version and errors out before any build. If you tagged
  the wrong commit, delete and re-tag rather than pushing a new version.
- **Forgetting `Cargo.lock`.** The bump is two files, not one. `--locked` will
  fail the Linux and macOS builds.
- **Tag without pushing, or pushed to `my`.** No workflow runs and there is no
  error to notice — only a missing release.
- **Signing is out of scope.** The DMGs are deliberately unsigned
  (`-unsigned.dmg`); the bundle ships `scripts/macos-installation.txt` as
  `README.txt` for the Gatekeeper workaround. Do not promise signed builds.

## Local packaging (optional dry run)

Both scripts refuse a `--version` that disagrees with Cargo, and require
`target/release/terminal-app` to already exist:

```bash
cargo build --locked --release -p terminal_app --bin terminal-app
bash scripts/package-linux-deb.sh --version X.Y.Z      # needs cargo-deb 3.7.0
bash scripts/package-macos-dmg.sh --version X.Y.Z --arch arm64
```

`package-macos-dmg.sh` uses only `sips`, `iconutil`, `plutil`, and `hdiutil`,
so it runs on macOS only. Use these when the workflow fails and you need to
reproduce a packaging error locally, not as a required release step.
