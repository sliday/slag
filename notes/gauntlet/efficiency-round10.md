# Round 10: portable fresh-checkout checks

## Causes and changes

The original Linux failure did not show a successful workspace write. The write failed with `OSError: [Errno 30] Read-only file system`. The test then failed because it expected `PermissionError`. Bubblewrap mounted the checkout read-only for root. The regression now checks the failed command, one executed test, and absence of the file. It accepts the kernel's read-only error text.

Strict bubblewrap sessions also drop all capabilities. Linux regressions check relative writes, absolute writes, truncation, symlink escapes, chmod, timestamps, and extended attributes. Private temporary files remain writable.

Landlock blocks file data writes but does not block chmod, timestamps, or extended attributes. Direct probes confirmed this under root and UID 1000. Strict read-only factories now return `Unsupported` before they run commands on Landlock. Slag warns once. It cannot cite those checks as successful read-only evidence. Ordinary Landlock confinement remains unchanged.

Debian's Python lacks ensurepip without `python3-venv`. The error spans two lines. Installer detection now normalizes whitespace. Installation tries venv, virtualenv, then pip with a private target. Each method uses a temporary clone. Virtualenv also keeps its app data there. Missing installer tools produce `install check skipped: <reason>` in the review context, warning event, and progress ledger. Package build failures remain errors. A skip adds no required proof command and permits no installed-console claim.

Two integration assertions also assumed platform-specific error text. They now accept Linux EROFS or normalize terminal wrapping.

## Verification

| Environment | Cargo result | Tests passed | Ignored |
| --- | --- | ---: | ---: |
| macOS, full suite | exit 0 | 1,258 | 1 |
| Linux root, bubblewrap, full suite | exit 0 | 1,260 | 1 |
| Linux UID 1000, bubblewrap, full suite | exit 0 | 1,260 | 1 |
| Linux root, Landlock-only sandbox tests | exit 0 | 35 | 0 |
| Linux UID 1000, Landlock-only sandbox tests | exit 0 | 35 | 0 |
| Linux UID 1000, bubblewrap, pip present and python3-venv absent | exit 0 | 11 | 0 |
| Linux root, Landlock-only checkout tests | exit 0 | 11 | 0 |
| Linux UID 1000, Landlock-only checkout tests | exit 0 | 11 | 0 |

Landlock-only checkout tests validate explicit refusal. They do not run product commands under a claimed read-only profile. Bubblewrap tests execute the checks under root and UID 1000.

The original root image has no installation tools. Its suite verifies graceful skips. The non-root wrapper then installs `python3-pip` without recommended packages. The checkout tests exercise the private target fallback without `python3-venv`.

macOS Clippy passed with exit 0 and `--all-targets -- -D warnings`. `git diff --check` passed. The main checkout stayed clean. No live API calls ran.

An interim Docker run hit the existing timeout test in `pipeline::fix::repo`. Both final full Linux runs passed it. No fix module changed.

Exact root command, using the lead's unchanged script:

```sh
docker run --rm --privileged \
  -v /Users/stas/Playground/slag/.claude/worktrees/lane-efficiency:/src:ro \
  -v /Users/stas/.claude/jobs/f353e9d3/tmp/linux-final/cache:/cache \
  -v /Users/stas/.claude/jobs/f353e9d3/tmp/linux-final/run.sh:/run.sh:ro \
  rust:1-bookworm bash /run.sh
```

The original script reports each Cargo exit through `PIPESTATUS[0]`. Its final results were `bwrap-exit=0` and `landlock-exit=0`.

Exact non-root command:

```sh
docker run --rm --privileged \
  -v /Users/stas/Playground/slag/.claude/worktrees/lane-efficiency:/src:ro \
  -v /Users/stas/.claude/jobs/f353e9d3/tmp/linux-final/cache:/cache \
  -v /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r10/nonroot.sh:/run.sh:ro \
  rust:1-bookworm bash /run.sh
```

The wrapper creates UID 1000, sets a writable HOME, and runs Cargo through `runuser`. It restores cache ownership afterward. Each Cargo invocation and the wrapper returned 0.

Exact native commands from the worktree:

```sh
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r10/tmp \
CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home \
CARGO_PROFILE_DEV_DEBUG=line-tables-only \
cargo test --offline --manifest-path slag-rs/Cargo.toml

CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home \
CARGO_PROFILE_DEV_DEBUG=line-tables-only \
cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
```

Evidence under `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r10/`:

- `linux-diagnose.log`: original EROFS failure.
- `linux-venv-diagnose.log`: Debian's wrapped ensurepip error.
- `probes/landlock-metadata.log`: root and UID 1000 metadata probes.
- `linux-root-final-result.log`: exact lead script, final run.
- `linux-nonroot-result.log`: non-root wrapper, final run.

Native logs: `slag-rs/target/round10-macos-final.log` and `slag-rs/target/round10-clippy-result.log`.

## Regression coverage

- `fresh_checkout_tests_cannot_write`
- `fresh_checkout_suite_can_use_private_tempfile`
- `bwrap_readonly_blocks_checkout_writes_and_allows_private_temp`
- `landlock_readonly_refuses_unprotected_metadata_writes`
- `readonly_scratch_refuses_unconfined_backend`
- `isolated_install_checks_console_without_workspace_residue`
- `isolated_install_falls_back_to_target_without_workspace_residue`
- `isolated_install_missing_tools_skips_without_a_proof_command`
- `isolated_install_package_failures_remain_errors`
- `install_tool_detection_does_not_hide_package_failures`
- `virtualenv_keeps_app_data_in_private_installation_directory`
- `skipped_install_check_reports_limits_without_an_unverified_verdict`
