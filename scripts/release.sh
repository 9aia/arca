#!/usr/bin/env bash
# Manual release — run by an operator on main (bupha watch, the old
# push trigger, is retired; releases are operator-run).
# The leading sleep is the debounce window: only the last invocation
# in a quiet window releases. Commits/tags are pushed to main.
#
# crates.io publish is retired: every cuora crate is an internal
# lib-only engine — org policy keeps every repo but tamias off
# crates.io, and the `cuora` command itself ships as the tamias pack
# (`libraries/cuora` → `tamias install 9aia:cuora@<ver>`), so a
# release is one workspace version bump + a `cuora-v<ver>` tag. The
# operator then publishes the pack itself
# (`tamias publish libraries/cuora`).
set -euo pipefail
# worktree-local secrets (gitignored); the service env file is canonical
[ -f ./.env ] && { set -a; source ./.env; set +a; }
sleep "${CUORA_RELEASE_DEBOUNCE:-300}"

git fetch origin --tags -q
last="$(git describe --tags --abbrev=0 2>/dev/null || true)"
if [ -z "$last" ]; then
    echo "no tags yet — tag the first release manually to seed the flow"; exit 0
fi
changed="$(git diff --name-only "${last}..HEAD")"

ws_version() { grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2; }

# release-worthy paths: workspace crates, the tamias pack payload,
# xtask, and the release machinery itself. Docs live on the orphan
# `docs` worktree branch — they never appear in this diff.
relevant="$(grep -E '^(crates/|libraries/|xtask/|campaigns/|rules/|Cargo\.(toml|lock)|scripts/release\.sh)' <<<"$changed" || true)"
if [ -z "$relevant" ]; then echo "no release-relevant changes since $last"; exit 0; fi

# One shared version: [workspace.package] version is the single
# source (members inherit `version.workspace = true`).
# `cargo set-version --workspace` rewrites the workspace.package
# version, the workspace.dependencies reqs, and Cargo.lock together.
old="$(ws_version)"
cargo set-version --workspace --bump patch
ver="$(ws_version)"

# keep the user-facing version refs in lockstep: the tamias pack
# manifest and the install example in the README.
sed -i "s/^version: \"${old}\"/version: \"${ver}\"/" libraries/cuora/tamias.yaml
sed -i "s|cuora@${old}|cuora@${ver}|g" README.md

git commit -aqm "release: bump cuora to ${ver} — tamias pack release"
git tag "cuora-v${ver}"
git push -q origin HEAD:main --tags

echo "released: cuora ${ver} (tag cuora-v${ver})"
echo "next: tamias publish libraries/cuora  # ship 9aia:cuora@${ver} to the org registry"
