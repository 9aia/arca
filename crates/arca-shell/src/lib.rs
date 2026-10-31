//! arca-shell — run a command in a working directory with extra env
//! vars injected. Two modes back `arca run`: a single string arg
//! goes through `bash -c` (shell syntax, quoting, pipes); multiple
//! argv are exec'd directly — `argv[0]` is the program, no shell
//! involved. Resolved secrets arrive as `(name, exposed-value)`
//! pairs and land only in the child's environment.

use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Run `bash -c <cmd>` with `workdir` as cwd; returns the exit
/// status. Stdio is inherited — callers that need captured output
/// should use [`output`].
pub fn run(workdir: &Path, cmd: &str) -> io::Result<ExitStatus> {
    run_with_env(workdir, cmd, &[])
}

/// Like [`run`] but injects `vars` into the child environment.
/// Values arrive already exposed — this crate never sees a `Secret`.
pub fn run_with_env(
    workdir: &Path,
    cmd: &str,
    vars: &[(String, String)],
) -> io::Result<ExitStatus> {
    let mut c = Command::new("bash");
    c.current_dir(workdir).args(["-c", cmd]);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.status()
}

/// Exec `argv` directly — `argv[0]` is the program, no shell, no
/// quoting surprises. The `arca run -- prog arg1 arg2` path.
/// Values arrive already exposed — this crate never sees a
/// `Secret`.
pub fn exec_with_env(
    workdir: &Path,
    argv: &[String],
    vars: &[(String, String)],
) -> io::Result<ExitStatus> {
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "exec: empty argv"))?;
    let mut c = Command::new(prog);
    c.current_dir(workdir).args(args);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.status()
}

/// Like [`run`] but captures stdout/stderr.
pub fn output(workdir: &Path, cmd: &str) -> io::Result<std::process::Output> {
    output_with_env(workdir, cmd, &[])
}

/// Like [`run_with_env`] but captures stdout/stderr.
pub fn output_with_env(
    workdir: &Path,
    cmd: &str,
    vars: &[(String, String)],
) -> io::Result<std::process::Output> {
    let mut c = Command::new("bash");
    c.current_dir(workdir).args(["-c", cmd]);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.output()
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

    #[test]
    fn injects_env_vars() {
        let d = tmpdir("env");
        let vars = [("ARCA_TEST_VAR".to_string(), "fake-injected".to_string())];
        let out = output_with_env(&d, "echo $ARCA_TEST_VAR", &vars)
            .unwrap_or_else(|_| std::process::exit(1));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fake-injected");
        // and the var didn't leak into *our* env
        assert!(std::env::var("ARCA_TEST_VAR").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn exec_argv_needs_no_shell() {
        let d = tmpdir("exec");
        let vars = [("ARCA_TEST_VAR".to_string(), "fake-via-argv".to_string())];
        // `printenv VAR` exits 0 and prints the value; with a shell
        // the quoting would matter — argv mode doesn't care.
        let st = exec_with_env(
            &d,
            &["printenv".to_string(), "ARCA_TEST_VAR".to_string()],
            &vars,
        )
        .unwrap_or_else(|_| std::process::exit(1));
        assert!(st.success());
        // missing program → io error, not a shell message
        assert!(exec_with_env(&d, &["definitely-not-a-binary".to_string()], &[]).is_err());
        assert!(exec_with_env(&d, &[], &[]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
