---
url: /cuora/explanation/worktree-model.md
---
# The worktree model

Why `project/` and `docs/` live as git worktrees of orphan
branches inside this repo — and what that buys.

## The shape

```text
cuora/
├── crates/ …           ← code branch (main)
├── project/            ← worktree of the orphan `project` branch
│   ├── TODO.md  BACKLOG.md  index.md  meta.toml
│   └── issues/  proposals/  plans/  adrs/  handoffs/
├── docs/               ← worktree of the orphan `docs` branch
│   └── this site — VitePress sources
└── .gitignore          ← ignores project/ and docs/
```

Both worktrees share the repo's `.git` object store but have
histories disjoint from `main` — orphan roots, no code
ancestors. To tools they're ordinary directories; to
`git worktree list` they're checkouts of their own branches.

## Why orphan branches

Non-code state moves at its own cadence. On orphan branches,
ledger and docs commits never touch the code history: no
`chore: handoff T-009` noise in `main`'s log, no PM or docs-site
diffs in code PRs. Both dirs are gitignored on `main` — the
worktrees physically can't leak into a code commit.

## Why worktrees (not just branches)

The content has to be *visible* — agents read `project/`, humans
edit `docs/` — without `git checkout` swapping the code tree.
Worktrees keep all three branches checked out at once.

## `project/` — the PM ledger

Owned by **myco**, not cuora: this repo dogfoods the stack's PM
tool. The `project` branch history is the audit trail — every
mutating `myco` verb is one atomic commit. `myco init` created
the branch and worktree; agents go through `myco` verbs rather
than hand-editing when a verb exists.

## `docs/` — this site

The `docs` branch holds the VitePress + Diátaxis sources;
generated pages (`reference/cli.md`, `reference/config.md`, the
crate map in `architecture.md`, `reference/crate-api.md`) are
rebuilt by `docs/gen-*.py` and `cuora gen-cli-docs` — never
hand-edited. `cargo xtask site` builds the site; pushes deploy to
GitHub Pages at `9aia.com/cuora` via `scripts/deploy-docs.sh`.

See [architecture](architecture.md) for the crates the code side
is made of.
