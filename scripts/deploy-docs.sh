#!/usr/bin/env bash
# Build the VitePress site and publish .vitepress/dist to the `gh-pages`
# branch. Runs identically by hand (from the docs/ worktree) and under
# the CI gate — the peregrin ci-gate campaign (bupha is retired) runs
# `bash scripts/deploy-docs.sh` in a detached worktree of the pushed
# docs sha, so cwd is the docs checkout root and
# `..` is NOT the main repo. Cargo work happens in CUORA_REPO (the real
# checkout); the gen-*.py scripts honor CUORA_ROOT for the same reason.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
DOCS="$(pwd)"
REPO="${CUORA_REPO:-$HOME/GitHub/9aia/workspace/repos/cuora}"
export CUORA_ROOT="$REPO"

echo "==> bun install"
bun install

CUORA_BIN="$(command -v cuora || true)"
CUORA_BIN="${CUORA_BIN:-$HOME/.cargo/bin/cuora}"
echo "==> reference/cli.md ($CUORA_BIN gen-cli-docs)"
"$CUORA_BIN" gen-cli-docs > reference/cli.md

echo "==> gen config + crate map (CUORA_ROOT=$REPO)"
python3 gen-config-docs.py
python3 gen-crate-map.py

echo "==> rustdoc → public/api (cargo doc in $REPO)"
python3 gen-api.py

echo "==> vitepress build"
bunx vitepress build

# Branch deploys still get a Jekyll pass unless .nojekyll is present.
touch .vitepress/dist/.nojekyll

echo "==> deploy .vitepress/dist → gh-pages"
bunx gh-pages -d .vitepress/dist -b gh-pages \
  -r git@github.com:9aia/cuora.git \
  -m "docs: deploy $(git rev-parse --short HEAD)"

echo "done — https://9aia.com/cuora/ serves origin/gh-pages"
