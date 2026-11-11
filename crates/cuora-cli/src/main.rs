//! cuora — compat CLI driver. The engine is the `cuora` lib
//! (src/lib.rs); this binary is just `cuora::run()` mapped to a
//! process exit code.

fn main() -> std::process::ExitCode {
    // Engine codes are 0/1 or a child's u8 exit status — anything
    // outside u8 collapses to failure rather than wrapping.
    std::process::ExitCode::from(u8::try_from(cuora::run()).unwrap_or(1))
}
