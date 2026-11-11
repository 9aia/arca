# cuora-cli

*Part of [cuora](https://github.com/9aia/cuora) — secrets broker for agents.*

The `cuora` engine as a library — `Cli`/`Cmd`, `command()`/`parse_from()`/`dispatch()`/`run_from()`, `--json` everywhere, `manifest`/`schema`. There is no `cuora` binary in this crate: the command ships as the tamias pack in `libraries/cuora` (a generated `bins[]` shim → `tamias-driver-cuora cli` → `cuora::run_from`). Engine e2e tests live in `crates/cuora-driver/tests/` and spawn `tamias-driver-cuora run`.
