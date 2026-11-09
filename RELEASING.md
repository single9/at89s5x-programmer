# Releasing

Releases are built and published by CI. A release is cut by pushing a `v*` tag
that matches the version in `Cargo.toml`.

## Versioning

[Semantic versioning](https://semver.org/), currently `0.y.z`:

- bump **z** for bug fixes and documentation-only changes that ship in the binary,
- bump **y** for new features, new platforms, or behaviour changes (command
  line, output format).

## Before releasing

- `main` is green on CI (format, Clippy, tests, builds on Linux, Windows,
  macOS).
- Anything that touches the USB or SPI code has been tried on real hardware
  (`selftest`, `info`, and a `flash` of `examples/blink_p1_0.hex`) on at least
  one platform, and the README / release-note wording about what is verified is
  still true.
- `README.md` and `README.zh-TW.md` are in sync (see `AGENTS.md`).

## Cut the release

```sh
scripts/release.sh X.Y.Z --dry-run   # checks only, changes nothing
scripts/release.sh X.Y.Z             # asks for confirmation, then publishes
```

The script requires a clean `main` that is in sync with `origin/main`, runs
`cargo fmt --check`, `cargo clippy -D warnings` and `cargo test`, then:

1. bumps `Cargo.toml` (and `Cargo.lock`), commits `Release X.Y.Z`, pushes,
2. waits for CI on that commit,
3. tags `vX.Y.Z` and pushes the tag,
4. waits for the Release workflow and prints the release URL and assets.

Manual equivalent: edit the version in `Cargo.toml`, run `cargo build`, commit,
push, wait for CI, then `git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z`.

## What CI publishes

`.github/workflows/release.yml` runs on a `v*` tag. It first checks that the tag
matches `Cargo.toml`, then runs the tests and builds:

| Platform | Asset |
|----------|-------|
| Linux x86_64 | `at89s5x-isp-vX.Y.Z-linux-x86_64.tar.gz` |
| Windows x86_64 | `at89s5x-isp-vX.Y.Z-windows-x86_64.zip` |
| macOS (Intel + Apple Silicon) | `at89s5x-isp-vX.Y.Z-macos-universal.tar.gz` |

Each asset has a `.sha256` next to it and contains the binary, both READMEs,
the license and `examples/`. The release notes are the text in `release.yml`
plus GitHub's generated change list. Edit that text when the verified platforms
change.

The macOS binary is ad-hoc signed but not notarized, so a browser download is
blocked until `xattr -d com.apple.quarantine at89s5x-isp` is run (documented in
the README).

## If a release fails

If the Release workflow fails before publishing, nothing is released. Fix the
cause on `main`, then move the tag:

```sh
git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z
# fix, commit, push, wait for CI, then tag and push again
```

If a release was published with a mistake, prefer a new patch version. To
remove one completely:

```sh
gh release delete vX.Y.Z --cleanup-tag --yes
```

Do not reuse a version number that users may already have downloaded.
