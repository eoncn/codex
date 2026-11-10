# Fork self-update: Windows x64 and Intel macOS

This document applies to **eoncn/codex dev only**. The `main` branch must remain an
unchanged snapshot of the official OpenAI Codex release.

## Release process

1. In Actions, manually run **Build Windows Codex Binary** on `dev`.
   Provide `build_number` (for example `1`). Save its successful run ID.
2. Manually run **Build macOS Intel Codex Package** on `dev`, with **the
   same** `build_number`. Save its successful run ID.
3. Manually run **Publish fork update release** on `dev`, providing:
   - `release_tag`: `fork-v0.167.0.1` for Codex version 0.167.0, build 1
   - `windows_run_id`: the Windows build run ID
   - `macos_run_id`: the Intel macOS build run ID
4. Publishing verifies that both assets match the tag's version and build,
   generates `codex-update.json` containing their SHA-256 checksums, and
   creates a non-prerelease GitHub Release marked latest. This workflow is
   manual and does not run on push.

Increment `build_number` for each new fork release **even when the underlying
Codex version is unchanged**. Never reuse a release tag.

## User installation and update behavior

Download the release's `codex-windows-x64.zip` or
`codex-macos-intel.tar.gz`, and **extract the complete package** (including
`bin/`, `codex-path/`, `codex-resources/`, and `codex-package.json`).

Run `bin/codex.exe` on Windows or `bin/codex` on Intel macOS. Copies of only
the executable are intentionally **not** eligible for in-place updates.

The release build embeds its fork build number. When the CLI starts, it checks
the latest `eoncn/codex` GitHub Release and shows a confirmation prompt if a
later version/build exists. The check is cached for up to 20 hours; a fresh
check is asynchronous, so a newly published release can show on the next
launch. Selecting **Update now** downloads the matching archive, verifies its
SHA-256 against `codex-update.json`, validates package metadata and executes
the new binary's `--version` smoke test. `codex update` checks immediately.

Only releases under `eoncn/codex` are considered; the fork build does not
invoke the official npm, Homebrew, or ChatGPT installation scripts.

- **macOS Intel**: the running CLI atomically renames the old package to a
  rollback directory, installs the staged package, validates again, and
  restores the previous directory if installation or validation fails.
- **Windows x64**: a separate system PowerShell helper waits for Codex to exit,
  swaps the staged and live directories, validates the new binary, and restores
  the backup if anything fails. It records the result in
  `updater.log` under the staging directory.

The parent directory of the installed package must be writable. If the
package is in a protected system directory, run an appropriately privileged
installer or relocate it to a user-writable location.

There is no silent application of updates: users must explicitly choose
Update now (or run `codex update`). New versions take effect after restarting
Codex. Updates never modify an npm or Homebrew installation.

## Local verification

```bash
python3 -m unittest scripts.tests.test_fork_release -v
```

Windows/macOS binaries must also pass their Actions build workflow. Static
validation alone is not equivalent to a successful native Cargo build.

## Limitations

- Supported: **Windows x64** and **Intel macOS x86_64** only.
- Updates use public GitHub Releases and require network access.
- SHA-256 verifies downloaded bytes against the public repository's release
  manifest. It is **not** a separate cryptographic maintainer signature.
- Automatic updates are limited to complete, writable package-layout
  installations. macOS packages are CLI tarballs, not notarized .app bundles.
