Complexity: standard — release script for four targets using known tools (rustup targets, cargo-zigbuild); verification by running each binary.

# Lane: release for every platform install.sh supports

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-release (git worktree, branch gauntlet/release). Work only here.

## Context

slag is a Rust CLI in `slag-rs/`. Releases ship through `slag-rs/dist-local/publish-release.sh`: `--build` builds THIS host's binary only (aarch64-apple-darwin) and writes `slag-<target>.tar.gz`, `slag-<target>` (raw binary) and `sha256.sum`; a second call publishes them to GitHub Releases (`sliday/slag`). `slag-rs/install.sh` (copied verbatim to `website/public/install.sh`, served at https://slag.dev/install.sh) maps `uname` to one of four targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu. Only the first one ships today, so installs on Linux and Intel Macs fail. `slag update` (`slag-rs/src/update.rs`) downloads the asset for its own target.

Tools on this machine: rustup (`~/.cargo/bin/rustup`, only aarch64-apple-darwin installed), zig (`/opt/homebrew/bin/zig`), docker. `cargo install cargo-zigbuild` is allowed. HTTP in slag uses reqwest with rustls (no OpenSSL).

## Goal

`publish-release.sh --build` produces correct, verified assets for all four targets on this Mac, and the publish step uploads all of them.

## Deliverables

1. `--build` builds all four targets: native for aarch64-apple-darwin, a rustup target for x86_64-apple-darwin, and `cargo zigbuild` for both Linux gnu targets (pick a glibc floor such as `.2.17` so the binary runs on older distros; say which). Each produces `slag-<target>.tar.gz` (single `slag` binary inside) and `slag-<target>`; one `sha256.sum` covers all eight files. Add `--target <triple>` to build one target only. Fail loudly with an actionable message when a prerequisite is missing (and say the install command).
2. Verification inside the script: `file` shows the right architecture for each binary; the host binary prints `slag --version` matching Cargo.toml. If docker is reachable, run each Linux binary's `--version` in `debian:stable-slim` with the right `--platform`; if not, print a clear skip note.
3. The publish path uploads every asset that `sha256.sum` lists.
4. `install.sh`: no change needed unless you find a real bug; if you change it, copy it byte-identical to `website/public/install.sh` in the same commit.
5. Check `src/update.rs` picks the right asset name on each target (read only; report any mismatch, do not edit).
6. Run `--build` for real once and report sizes and the verification output. Do NOT commit the built binaries or tarballs (add nothing to git under `dist-local/` except the script). Do NOT publish.

## Paths you own (edit nothing else)

`slag-rs/dist-local/publish-release.sh`, optionally `slag-rs/install.sh` + `website/public/install.sh` (byte-identical pair), and a short release section in `CLAUDE.md` if the procedure changes.

## Hard rules

- No `gh release` calls, no `git push`, no tags.
- Prose (comments, messages): no em dashes, active voice, short sentences. Match the script's existing comment style.
- Commit on branch `gauntlet/release` with a message like `release: build every target install.sh knows`.

When done, list the files you changed, the exact commands that verify the work, and the build/verification output summary.
