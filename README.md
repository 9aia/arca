# arca docs

VitePress + Diátaxis site — served from the `gh-pages` branch at
`9aia.com/arca`. Deploys are driven by bupha: a push to this
branch runs `scripts/deploy-docs.sh` in a detached worktree of
the pushed sha (gen → `cargo doc` → `vitepress build` →
`bunx gh-pages`). Pages is a legacy branch deploy — pushing
`gh-pages` is the whole deploy; there is no Actions workflow.

The detached worktree has no sibling `main` checkout, so cargo
work happens in `$ARCA_REPO` (default `~/GitHub/9aia/arca`) and
the `gen-*.py` scripts resolve sources via `ARCA_ROOT` instead of
`..`. Run `bash scripts/deploy-docs.sh` by hand for the same
pipeline locally.

From the repo root: `cargo xtask site` runs `docs:gen` +
`vitepress build`. Generated pages — `reference/cli.md`
(`arca gen-cli-docs`), `reference/config.md`, the crate map in
`explanation/architecture.md`, `reference/crate-api.md` — come
from `gen-*.py` / the CLI, never hand-edit. `bun run docs:api`
rebuilds the rustdoc under `public/api/` (gitignored — CI
regenerates it).
