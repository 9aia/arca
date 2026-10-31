//! arca-shell — run an arbitrary command in a working directory via
//! `bash -c`. Git plumbing lives in `arca-git`; this crate is for the
//! other shell-outs a PM tool needs (hooks, importers, user scripts).

use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Run `bash -c <cmd>` with `workdir` as cwd; returns the exit
/// status. Stdio is inherited — callers that need captured output
/// should use [`output`].
pub fn run(workdir: &Path, cmd: &str) -> io::Result<ExitStatus> {
    Command::new("bash")
        .current_dir(workdir)
        .args(["-c", cmd])
        .status()
}

/// Like [`run`] but captures stdout/stderr.
pub fn output(workdir: &Path, cmd: &str) -> io::Result<std::process::Output> {
    Command::new("bash")
        .current_dir(workdir)
        .args(["-c", cmd])
        .output()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("arca-shell-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap_or_default();
        d
    }

    #[test]
    fn runs_in_worktree() {
        let d = tmpdir("run");
        let st = run(&d, "exit 0").unwrap_or_else(|_| std::process::exit(1));
        assert!(st.success());
        let st = run(&d, "exit 3").unwrap_or_else(|_| std::process::exit(1));
        assert_eq!(st.code(), Some(3));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn captures_output() {
        let d = tmpdir("out");
        let out = output(&d, "echo hi; pwd").unwrap_or_else(|_| std::process::exit(1));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("hi"));
        assert!(text.contains(d.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
