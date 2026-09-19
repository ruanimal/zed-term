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
| Assets | 1 deb (`zedterm-X.Y.Z-linux-amd64.deb`) + 2 DMGs (`macos-x86_64`, `macos-arm64`), exactly 3 |

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

### 5. Watch and verify

```bash
gh run list --workflow release.yml --limit 3
gh run watch                # follow the run started by the tag push
gh release view vX.Y.Z      # after publish succeeds
```

The run has four jobs: `prepare` (validates tag == Cargo version), `linux`,
`macos` (x86_64 + arm64 matrix), `publish`. The `publish` job downloads
`zedterm-*` artifacts and hard-fails unless it finds exactly 3 files, then
creates the GitHub release with generated notes and `draft: false`.

Success is a published release with all three assets attached. Report the
release URL and asset names.

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
