//! The policy decision point — an *external* engine (peregrin's
//! job, never arca's) that `arca serve` consults before every
//! vault-touching op.
//!
//! One driver: `exec`. The configured command runs via `sh -c`
//! once per decision; the [`PdpRequest`] goes in on stdin as one
//! JSON line, the verdict comes back on stdout —
//! `{"allow": bool, "reason"?: string, "obligations"?: [string]}`.
//! Spawn failure, non-zero exit, missing `allow`, or unparseable
//! output are all [`Decision::Unreachable`] — what that means is
//! the caller's `[serve.pdp] allow_on_error` flag.
//!
//! The exec driver keeps the binary dependency-light and sync —
//! no HTTP client, no async runtime, no daemon protocol. A PDP
//! that wants a persistent socket can put `socat`/`nc` in its
//! exec line; the wire stays the same.

use std::io::Write;
use std::process::{Command, Stdio};

use arca_core::PdpConfig;
use serde::Serialize;

/// What serve asks the PDP — one materialization decision.
#[derive(Debug, Serialize)]
pub struct PdpRequest<'a> {
    /// The claimed actor (self-asserted, advisory — attestation is
    /// where real proof would ride).
    pub actor: &'a str,
    /// The requester's opaque identity claim, passed through
    /// verbatim. arca never interprets it; the PDP validates (or
    /// ignores) it. Real signing is peregrin's side — a later ADR.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<&'a str>,
    /// The op being decided (`get`/`put`/`rm`/`rotate`).
    pub op: &'a str,
    /// The env the op resolves under (pin-aware).
    pub env: &'a str,
    /// The resolved ref path (env pin stripped).
    #[serde(rename = "ref")]
    pub rref: &'a str,
}

/// What the PDP answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The op may run; optional reason/obligations ride along.
    Allow {
        /// PDP-supplied rationale.
        reason: Option<String>,
        /// Conditions on the allow (`allow-with-conditions`).
        obligations: Option<Vec<String>>,
    },
    /// The op is refused.
    Deny {
        /// PDP-supplied rationale.
        reason: Option<String>,
    },
    /// No verdict was obtained — spawn failed, non-zero exit, or
    /// the output wasn't a verdict. The string is diagnostics,
    /// never policy input.
    Unreachable(String),
}

/// The exec-driver PDP — one `sh -c` spawn per decision.
pub struct ExecPdp {
    cmd: String,
    allow_on_error: bool,
}

impl ExecPdp {
    /// From `[serve.pdp]` config.
    pub fn new(cfg: &PdpConfig) -> Self {
        ExecPdp {
            cmd: cfg.exec.clone(),
            allow_on_error: cfg.allow_on_error,
        }
    }

    /// `[serve.pdp] allow_on_error`.
    pub fn allow_on_error(&self) -> bool {
        self.allow_on_error
    }

    /// Ask the PDP. Never panics, never blocks on parse — a
    /// malformed verdict is just another kind of unreachable.
    pub fn decide(&self, req: &PdpRequest) -> Decision {
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(&self.cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The PDP's stderr is not the audit wire — null it so a
            // chatty decider can't pollute serve's event stream.
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Decision::Unreachable(format!("pdp spawn failed: {e}")),
        };
        // Serializing this POD request cannot fail — the expect
        // documents the invariant, it doesn't guard a real path.
        #[allow(clippy::unwrap_used)]
        let line = serde_json::to_string(req).expect("pdp request serializes");
        if let Some(mut stdin) = child.stdin.take() {
            // A PDP that closes stdin early (decided from argv
            // alone) must not crash us — write errors are ignored.
            let _ = writeln!(stdin, "{line}");
        }
        match child.wait_with_output() {
            Err(e) => Decision::Unreachable(format!("pdp wait failed: {e}")),
            Ok(out) if !out.status.success() => {
                Decision::Unreachable(format!("pdp exited {}", out.status))
            }
            Ok(out) => parse_verdict(&out.stdout),
        }
    }
}

/// The verdict shape the PDP owes on stdout.
#[derive(Debug, serde::Deserialize)]
struct Verdict {
    allow: bool,
    reason: Option<String>,
    obligations: Option<Vec<String>>,
}

fn parse_verdict(stdout: &[u8]) -> Decision {
    match serde_json::from_slice::<Verdict>(stdout) {
        Ok(v) if v.allow => Decision::Allow {
            reason: v.reason,
            obligations: v.obligations,
        },
        Ok(v) => Decision::Deny { reason: v.reason },
        Err(e) => Decision::Unreachable(format!("pdp verdict unparseable: {e}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::PdpConfig;

    fn pdp(exec: &str, allow_on_error: bool) -> ExecPdp {
        ExecPdp::new(&PdpConfig {
            exec: exec.into(),
            allow_on_error,
        })
    }

    fn req() -> PdpRequest<'static> {
        PdpRequest {
            actor: "agent:test",
            attestation: Some("fake-attestation"),
            op: "get",
            env: "dev",
            rref: "a/b",
        }
    }

    #[test]
    fn allow_deny_and_obligations_parse() {
        let d = pdp(
            "printf '%s' '{\"allow\":true,\"reason\":\"ok\",\"obligations\":[\"redact\"]}'",
            false,
        )
        .decide(&req());
        assert_eq!(
            d,
            Decision::Allow {
                reason: Some("ok".into()),
                obligations: Some(vec!["redact".into()])
            }
        );
        let d = pdp(
            "printf '%s' '{\"allow\":false,\"reason\":\"no prod\"}'",
            false,
        )
        .decide(&req());
        assert_eq!(
            d,
            Decision::Deny {
                reason: Some("no prod".into())
            }
        );
        // a bare allow carries no extras
        let d = pdp("printf '%s' '{\"allow\":true}'", false).decide(&req());
        assert_eq!(
            d,
            Decision::Allow {
                reason: None,
                obligations: None
            }
        );
    }

    #[test]
    fn every_failure_shape_is_unreachable() {
        // unspawnable command
        assert!(matches!(
            pdp("/nonexistent-pdp-binary-xyz", false).decide(&req()),
            Decision::Unreachable(_)
        ));
        // non-zero exit
        assert!(matches!(
            pdp("exit 3", false).decide(&req()),
            Decision::Unreachable(_)
        ));
        // garbage on stdout
        assert!(matches!(
            pdp("printf 'not json'", false).decide(&req()),
            Decision::Unreachable(_)
        ));
        // missing `allow` key
        assert!(matches!(
            pdp("printf '%s' '{\"reason\":\"hmm\"}'", false).decide(&req()),
            Decision::Unreachable(_)
        ));
        // a pdp that closes stdin without reading doesn't kill us
        assert_eq!(
            pdp("printf '%s' '{\"allow\":true}'", false).decide(&req()),
            Decision::Allow {
                reason: None,
                obligations: None
            }
        );
    }

    #[test]
    fn the_request_rides_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let captured = dir.path().join("req.json");
        let cmd = format!(
            "cat > {}; printf '%s' '{{\"allow\":true}}'",
            captured.display()
        );
        assert_eq!(
            pdp(&cmd, false).decide(&req()),
            Decision::Allow {
                reason: None,
                obligations: None
            }
        );
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&captured).unwrap()).unwrap();
        assert_eq!(sent["actor"], "agent:test");
        assert_eq!(sent["attestation"], "fake-attestation");
        assert_eq!(sent["op"], "get");
        assert_eq!(sent["env"], "dev");
        assert_eq!(sent["ref"], "a/b");
    }

    #[test]
    fn allow_on_error_flag_passthrough() {
        assert!(pdp("x", true).allow_on_error());
        assert!(!pdp("x", false).allow_on_error());
    }
}
