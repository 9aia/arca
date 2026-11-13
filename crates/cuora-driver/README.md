# cuora-driver — `tamias-driver-cuora`

The [pavo](../../../pavo) view driver for cuora — answers the
`manifest` / `views` / `view` / `query` ndjson JSON-RPC wire
(`{"op":…}` → `{"ok":true,"result":…}` / `{"ok":false,"error":…}`,
same shape as `cuora serve`) as a `serve` subcommand over stdio,
plus `manifest`/`capabilities` argv probes for discovery.

This crate replaced `cuora-tui` (`cuora ui`, ratatui) — pavo owns
the interactive surface now; the driver is a pure reader over
`cuora-ops`, exactly like the TUI was.

## Secrets safety — read this first

**No view, query, or param can render a secret value.** This is a
hard requirement, not a default:

- `refs`/`tree`/`secret`/`declared`/`vaults`/`envs`/`status` carry
  names, refs, versions, timestamps, kinds, and routes only.
- `list_records` decrypts records to reach bookkeeping fields;
  `SecretRecord::value` is dropped uninspected when each row is
  built. `Secret`'s redacting `Display`/`Debug` is never invoked.
- `secret` reads a record for `stored`/`version`/timestamps — it
  never calls `Ops::get_in`, so **no materialization audit event
  fires** for browsing.
- There is deliberately **no reveal op**. The TUI's audited
  `r`→`r` two-step value reveal has no pavo equivalent: values
  stay behind `cuora get`/`cuora run`/`cuora env`/`cuora serve`,
  the only edges that expose plaintext.
- Views are safe to screenshot and share — that's the point.

One caveat worth knowing: remote (`cf`) vaults resolve scoped
per-env tokens internally, which emits ordinary `get` audit events
on stderr for the *token* refs — the same events `cuora list`
produces. Token *names* may appear in audit; values never do.

## Target resolution

Each `view`/`query` builds `cuora_ops::Ops` once per request —
the same resolution the CLI uses:

1. Request params `config` / `env` / `actor` (e.g.
   `pavo view cuora:refs --param env=prod --param config=/path/cuora.toml`)
2. `--config`-equivalent search: `$CUORA_CONFIG` → `./cuora.toml`
   → `~/.config/cuora/cuora.toml`
3. `$CUORA_ENV` → `dev`; `$CUORA_ACTOR` → `$USER` → `anon`

`manifest`/`views`/`capabilities` never touch vault state.

Reads go **in-process through `cuora-ops`** — not through
`cuora serve`. Rationale: every view is a metadata read the ops
layer already supports (`list`, `list_records`, `check`,
`open_vault`), and staying in-process keeps the driver a pure
reader with no subprocess lifecycle. `cuora serve` remains the
write/embed surface; nothing here proxies it.

## Views

| View | Route | Params | Data |
| --- | --- | --- | --- |
| `status` | `/cuora/status` | — | kv: config source, env, actor, vault, counts |
| `vaults` | `/cuora/vaults` | — | table: name, kind, path/url, cache, routed envs |
| `envs` | `/cuora/envs` | — | table: env → vault routing (the TUI tab strip) |
| `refs` | `/cuora/refs` | `env` | table: ref, version, created, rotated — one env |
| `tree` | `/cuora/tree` | — | env → refs hierarchy, all envs |
| `secret` | `/cuora/secret` | `ref` (required) | kv: env, vault, version, timestamps, stored? |
| `declared` | `/cuora/declared` | — | table: `[secrets]` var → ref, resolved env/vault |
| `check` | `/cuora/check` | — | `cuora check` as a view: vault probe + resolution + problems |

`env`/`config`/`actor` params feed `Ops::load` on every op, so
`--param env=prod` behaves like `cuora --env prod`. `refs`
lists the *selected* env; `tree` covers all of them; `secret`
honors `cuora://<env>/…` pins via `Ops::resolve`.

Contributions: navbar → `/cuora/refs`, dashboard + statusbar →
`/cuora/status` (`summary` is config-only — safe to poll, no
vault I/O).

## Degradation

A bad `--config`, an unopenable vault, or a missing `ref` param
answers `ok:true` with `{"error": …}` inside the view's `data` —
renderers show the failure inline, the same contract the TUI used
for its "vault not readable" line. Wire-level errors (`ok:false`)
are reserved for malformed requests and unknown ops/views/queries.

## TUI → view mapping (retirement checklist)

| `cuora-tui` surface | pavo equivalent |
| --- | --- |
| env tabs (`dev`/`staging`/`prod` + `[envs]` customs) | `cuora:envs` table, `cuora:tree` |
| ref list + `/` filter | `cuora:refs` table (host-side filtering) |
| detail pane (ref, version, `created_at`/`rotated_at`) | `cuora:secret` (`--param ref=`) |
| detail pane vault kind/location | `cuora:vaults` / `cuora:envs` |
| "vault not readable" inline error | `{"error": …}` payloads in every view |
| `R` reload | every `view`/`query` is a fresh `Ops` load |
| `r`→`r` audited value reveal | **not ported — values never render** |
| `j/k`/`tab`/`?`/`q` keys | pavo host chrome owns interaction |
| `cuora check` verb | `cuora:check` view |
| `[secrets]` declared surface | `cuora:declared` table |
| `cuora ui` itself | this driver |

## Install / discovery

`cargo install --path crates/cuora-driver` puts
`tamias-driver-cuora` on `PATH`; pavo also discovers it under
`<tamias-home>/drivers/` (`$TAMIAS_HOME` or
`~/.local/share/tamias/drivers/`).

## Conformance

`pavo-testkit`'s `check_driver` suite passes: manifest advertises
`serve`+`pavo`, `views` matches the manifest, every declared view
answers a `ViewResult` with all bindings populated, every binding
answers a `QueryResult`, malformed lines get `ok:false`, and the
session survives. Unit tests additionally assert the
no-value-bearing-query invariant structurally.

## Sealed packs (tamias ADR-0004)

Tamias carries `sealed:` packs as ciphertext only — this driver owns
everything that touches keys or plaintext (`src/sealed.rs`):

- `seal --in <dir>|NAME=PATH… --out <payload> --recipient age1…`
  (or `--recipients <file>` / `--keyfile`) — encrypts files into
  `sealed/` (`recipients` + `<name>.age`) and prints the `sealed:`
  manifest block; `--manifest <tamias.yaml>` merges it into the pack
  manifest in place (comments preserved, result validated before it
  lands).
- `open <pack> <name>` / `open cuora://<pack>/<name>` — decrypt one
  secret to stdout or `--out` (0600). `--dir <payload>` is the
  `tamias driver exec` dispatch form. `<pack>` may pin a version —
  `cuora://<pack>@<version>/<name>` — the tamias `name@version`
  spelling; several same-name installs without a pin are an
  ambiguity error, never a silent pick.
- `materialize <pack> --out <dir>` — decrypt every declared secret
  into a fresh dir (0600 files, refuses a non-empty target).
- `recipients <pack>` — the pack's public age recipients, marking
  the local keyfile's match.
- `packs` — installed sealed packs in the tamias store.
- `install`/`deploy`/`uninstall` — lifecycle no-ops for `type:
  secrets` packs (`capabilities.types` claims the kind); a
  non-sealed payload is refused loudly.

Identity is the cuora age keyfile (`~/.config/cuora/identity.txt`,
0600, `$CUORA_IDENTITY_FILE` / `--keyfile` override) — the same file
`cuora init` manages. Store resolution mirrors tamias:
`$TAMIAS_HOME/store` → XDG → `~/.local/share/tamias/store` →
`./.tamias/store` (`--store` overrides). Every open/materialize
emits an `Op::Get` audit event (`env: "sealed"`); plaintext lives
only in `Zeroizing` buffers between read and write — nothing logs
values.
