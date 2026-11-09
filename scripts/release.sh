#!/usr/bin/env bash
# Cut a release: bump the version, check, tag, push, and wait for CI to publish.
#
#   scripts/release.sh 0.2.1            # asks before anything is pushed
#   scripts/release.sh 0.2.1 --dry-run  # only run the checks, change nothing
#
# See RELEASING.md for the full process.
set -euo pipefail

version="${1:-}"
mode="${2:-}"
repo="single9/at89s5x-programmer"

die() { echo "error: $*" >&2; exit 1; }
step() { echo; echo "==> $*"; }

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: $0 X.Y.Z [--dry-run]"
[[ -z "$mode" || "$mode" == "--dry-run" ]] || die "unknown option: $mode"
dry=false; [[ "$mode" == "--dry-run" ]] && dry=true
tag="v$version"

cd "$(git rev-parse --show-toplevel)"
command -v gh >/dev/null || die "the GitHub CLI (gh) is required"

step "Checking the repository state"
[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || die "not on main"
[[ -z "$(git status --porcelain)" ]] || die "the working tree is not clean"
git fetch origin main --tags --quiet
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || die "main is not in sync with origin/main"
git rev-parse "$tag" >/dev/null 2>&1 && die "tag $tag already exists"

current=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
echo "current version: $current, new version: $version"
[[ "$current" != "$version" ]] || die "Cargo.toml is already at $version"

step "Running the checks"
cargo fmt --check
cargo clippy --locked -- -D warnings
cargo test --locked

if $dry; then
  echo; echo "Dry run finished: the checks pass. Nothing was changed."
  exit 0
fi

echo
read -r -p "Release $tag now? This publishes it. [y/N] " answer
[[ "$answer" == "y" || "$answer" == "Y" ]] || die "aborted"

step "Bumping the version"
awk -v v="$version" '!done && /^version = "/ { print "version = \"" v "\""; done=1; next } { print }' \
  Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
cargo build --locked >/dev/null 2>&1 || cargo build >/dev/null   # refreshes Cargo.lock
git add Cargo.toml Cargo.lock
git commit -m "Release $version"
git push origin main

step "Waiting for CI on main"
sleep 10
ci=$(gh run list --repo "$repo" --workflow CI --branch main --limit 1 --json databaseId -q '.[0].databaseId')
gh run watch "$ci" --repo "$repo" --exit-status || die "CI failed; fix it before tagging (the version commit is already pushed)"

step "Tagging $tag"
git tag -a "$tag" -m "$tag"
git push origin "$tag"

step "Waiting for the release workflow"
sleep 10
rel=$(gh run list --repo "$repo" --workflow Release --limit 1 --json databaseId -q '.[0].databaseId')
gh run watch "$rel" --repo "$repo" --exit-status || die "the release workflow failed; see RELEASING.md for how to retry"

step "Published"
gh release view "$tag" --repo "$repo" --json url,assets -q '.url, (.assets[].name)'
