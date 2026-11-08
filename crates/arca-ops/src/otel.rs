//! OTLP/HTTP (JSON) trace export for audit events — the
//! observability wire that sits beside stderr ndjson.
//!
//! Enabled by `OTEL_EXPORTER_OTLP_ENDPOINT` (standard OTel env;
//! `/v1/traces` is appended unless the endpoint already ends with
//! it). `OTEL_EXPORTER_OTLP_HEADERS` carries `k=v,k2=v2` (values
//! percent-decoded per spec), `OTEL_EXPORTER_OTLP_TIMEOUT` is
//! milliseconds (default 10s), `OTEL_SERVICE_NAME` names the
//! resource (default `arca`), and `OTEL_SDK_DISABLED=true` turns
//! the whole export off.
//!
//! Each [`AuditEvent`] becomes one internal span named
//! `arca.<op>` — instant (start == end) — with `arca.*`
//! attributes. Export is best-effort: a failed POST warns once on
//! stderr and never fails the op — audit must not take the broker
//! down, same posture as [`super::audit::emit`].
//!
//! Sync only (ureq, no async runtime) per AGENTS.md. The wire is
//! OTLP/HTTP JSON — hand-rolled payload, no opentelemetry crates,
//! keeping the dep tree light.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use super::audit::{AuditEvent, Outcome};

/// Resolved once per process — env doesn't change mid-run.
struct Otel {
    /// Base endpoint, e.g. `http://localhost:4318`.
    endpoint: String,
    /// `OTEL_EXPORTER_OTLP_HEADERS`, decoded.
    headers: Vec<(String, String)>,
    /// `service.name` resource attribute.
    service: String,
    /// `OTEL_EXPORTER_OTLP_TIMEOUT` (ms), spec default 10s.
    timeout: Duration,
}

static CONFIG: OnceLock<Option<Otel>> = OnceLock::new();

fn config() -> Option<&'static Otel> {
    CONFIG
        .get_or_init(|| {
            let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            otel_from(
                get("OTEL_EXPORTER_OTLP_ENDPOINT").as_deref(),
                get("OTEL_EXPORTER_OTLP_HEADERS").as_deref(),
                get("OTEL_SERVICE_NAME").as_deref(),
                get("OTEL_EXPORTER_OTLP_TIMEOUT").as_deref(),
                get("OTEL_SDK_DISABLED").as_deref(),
            )
        })
        .as_ref()
}

/// Whether OTLP export is configured — `audit::emit` buffers only
/// when this is true, so the no-endpoint path allocates nothing.
pub(crate) fn enabled() -> bool {
    config().is_some()
}

/// Resolve the config from raw env values — the testable core;
/// `None` endpoint or `OTEL_SDK_DISABLED` disables export.
fn otel_from(
    endpoint: Option<&str>,
    headers: Option<&str>,
    service: Option<&str>,
    timeout_ms: Option<&str>,
    sdk_disabled: Option<&str>,
) -> Option<Otel> {
    if sdk_disabled.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1")) {
        return None;
    }
    let endpoint = endpoint?.trim_end_matches('/').to_string();
    if endpoint.is_empty() {
        return None;
    }
    // Same policy as cf vault base URLs: `http://` only to a
    // loopback collector — these spans carry secret *names*.
    if !endpoint_allowed(&endpoint) {
        arca_notice::note(
            "OTEL_EXPORTER_OTLP_ENDPOINT uses http:// to a non-loopback \
             host — export disabled (use https:// or a loopback collector)",
        );
        return None;
    }
    Some(Otel {
        endpoint,
        headers: parse_headers(headers.unwrap_or("")),
        service: service.unwrap_or("arca").to_string(),
        timeout: timeout_ms
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(10)),
    })
}

/// `https://` anywhere, `http://` only to loopback — mirrors the
/// base-URL rule enforced for cf vaults and deploy sinks.
fn endpoint_allowed(endpoint: &str) -> bool {
    let Some(rest) = endpoint.strip_prefix("http://") else {
        return true; // https:// or a scheme we don't police
    };
    let authority = rest.split('/').next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(b) = hostport.strip_prefix('[') {
        b.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// `k=v,k2=v2` — spec says values may be percent-encoded; decode
/// both sides leniently (a malformed pair is skipped, never fatal).
fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = percent_decode(k.trim());
            let v = percent_decode(v.trim());
            (!k.is_empty() && !v.is_empty()).then_some((k, v))
        })
        .collect()
}

/// Minimal `%XX` decode — leaves non-escape bytes (and a stray
/// `%`) untouched.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `{endpoint}/v1/traces` — unless the endpoint already carries
/// the signal path (some collectors are configured per-signal).
fn trace_url(endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    if base.ends_with("/v1/traces") {
        base.to_string()
    } else {
        format!("{base}/v1/traces")
    }
}

/// POST the buffered events as one `ExportTraceServiceRequest`.
/// Any failure is the caller's warning, not an op failure.
pub(crate) fn export(events: &[AuditEvent]) -> Result<(), String> {
    let cfg = config().ok_or("otel export disabled")?;
    let body = serde_json::to_string(&trace_payload(cfg, events))
        .map_err(|e| format!("serialize: {e}"))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(cfg.timeout))
        .build()
        .into();
    let mut rb = agent.post(&trace_url(&cfg.endpoint));
    for (k, v) in &cfg.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    let mut res = rb
        .config()
        .http_status_as_error(false)
        .build()
        .content_type("application/json")
        .send(body)
        .map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    // Drain a bounded body so the connection can be reused; the
    // payload is a collector ack, never interesting.
    let _ = res
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} from {}", trace_url(&cfg.endpoint)));
    }
    Ok(())
}

/// The OTLP/HTTP JSON payload — pure, so tests pin the wire shape
/// without a collector.
fn trace_payload(cfg: &Otel, events: &[AuditEvent]) -> Value {
    let spans: Vec<Value> = events.iter().map(span_json).collect();
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": { "stringValue": cfg.service },
                }],
            },
            "scopeSpans": [{
                "scope": {
                    "name": "arca-ops",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "spans": spans,
            }],
        }],
    })
}

/// One audit event → one instant internal span. `traceId`/`spanId`
/// are random per event — audit events have no parent context.
fn span_json(ev: &AuditEvent) -> Value {
    let nanos = ev.ts.unix_timestamp_nanos().to_string();
    let mut attrs = vec![
        json!({"key": "arca.op", "value": {"stringValue": ev.op.as_str()}}),
        json!({"key": "arca.env", "value": {"stringValue": ev.env}}),
        json!({"key": "arca.actor", "value": {"stringValue": ev.actor}}),
        json!({"key": "arca.outcome", "value": {"stringValue": outcome_str(ev.outcome)}}),
    ];
    if let Some(rref) = &ev.rref {
        attrs.push(json!({"key": "arca.rref", "value": {"stringValue": rref}}));
    }
    if let Some(p) = &ev.policy {
        attrs.push(json!({"key": "arca.policy.allow", "value": {"boolValue": p.allow}}));
        if let Some(reason) = &p.reason {
            attrs.push(json!({"key": "arca.policy.reason", "value": {"stringValue": reason}}));
        }
        if let Some(obs) = &p.obligations {
            attrs.push(json!({
                "key": "arca.policy.obligations",
                "value": {"stringValue": obs.join(",")},
            }));
        }
    }
    let mut span = json!({
        "traceId": rand_hex(32),
        "spanId": rand_hex(16),
        "name": format!("arca.{}", ev.op.as_str()),
        "kind": "SPAN_KIND_INTERNAL",
        "startTimeUnixNano": nanos,
        "endTimeUnixNano": nanos,
        "attributes": attrs,
    });
    if ev.outcome == Outcome::Error {
        span["status"] = json!({"code": "STATUS_CODE_ERROR"});
    }
    span
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Denied => "denied",
        Outcome::Error => "error",
    }
}

/// `digits` hex chars from a per-process random state plus a
/// counter — trace/span ids need uniqueness, not secrecy.
fn rand_hex(digits: usize) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    // Fresh keys per call — `RandomState::new` draws OS entropy, so
    // ids differ across processes and calls.
    let state = RandomState::new();
    let mut out = String::with_capacity(digits);
    let mut i = 0u64;
    while out.len() < digits {
        let mut h = state.build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out.push_str(&format!("{:016x}", h.finish()));
        i += 1;
    }
    out.truncate(digits);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::audit::PolicyVerdict;

    #[test]
    fn disabled_without_endpoint_or_when_sdk_disabled() {
        assert!(otel_from(None, None, None, None, None).is_none());
        assert!(otel_from(Some(""), None, None, None, None).is_none());
        assert!(otel_from(Some("http://x:4318"), None, None, None, Some("true")).is_none());
        assert!(otel_from(Some("http://x:4318"), None, None, None, Some("1")).is_none());
        assert!(otel_from(Some("https://x:4318"), None, None, None, Some("false")).is_some());
    }

    #[test]
    fn http_endpoints_must_be_loopback() {
        // secret names ride these spans — plaintext http off-box
        // is refused, same policy as cf vault base URLs.
        assert!(otel_from(Some("http://collector:4318"), None, None, None, None).is_none());
        assert!(otel_from(Some("http://evil.example.com"), None, None, None, None).is_none());
        assert!(otel_from(Some("http://user@collector:4318"), None, None, None, None).is_none());
        for ok in [
            "http://localhost:4318",
            "http://127.0.0.1:4318",
            "http://[::1]:4318",
            "https://collector.example.com",
        ] {
            assert!(
                otel_from(Some(ok), None, None, None, None).is_some(),
                "{ok}"
            );
        }
    }

    #[test]
    fn headers_parse_pairs_and_decode() {
        let h = parse_headers("Authorization=Bearer%20abc, x-tenant=a%2Fb, bad, =nokey");
        assert_eq!(
            h,
            vec![
                ("Authorization".to_string(), "Bearer abc".to_string()),
                ("x-tenant".to_string(), "a/b".to_string()),
            ]
        );
        assert!(parse_headers("").is_empty());
    }

    #[test]
    fn trace_url_appends_v1_traces_once() {
        assert_eq!(
            trace_url("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            trace_url("http://h:4318/v1/traces"),
            "http://h:4318/v1/traces"
        );
        assert_eq!(trace_url("http://h:4318/"), "http://h:4318/v1/traces");
    }

    #[test]
    fn payload_is_otlp_json_with_arca_attrs() {
        let cfg = otel_from(
            Some("https://otel:4318"),
            Some("k=v"),
            Some("arca-test"),
            Some("2500"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.timeout, Duration::from_millis(2500));
        let ev = AuditEvent::op(
            "devin",
            arca_core::Op::Get,
            Some("billing/stripe"),
            "prod",
            Outcome::Denied,
        )
        .with_policy(Some(PolicyVerdict {
            allow: false,
            reason: Some("no prod".into()),
            obligations: None,
        }));
        let v = trace_payload(&cfg, &[ev]);
        let s = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["name"], "arca.get");
        assert_eq!(s["kind"], "SPAN_KIND_INTERNAL");
        assert_eq!(s["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(s["spanId"].as_str().unwrap().len(), 16);
        let attrs = s["attributes"].as_array().unwrap();
        let get = |k: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
                .unwrap()
        };
        assert_eq!(get("arca.op")["stringValue"], "get");
        assert_eq!(get("arca.rref")["stringValue"], "billing/stripe");
        assert_eq!(get("arca.outcome")["stringValue"], "denied");
        assert_eq!(get("arca.policy.allow")["boolValue"], false);
        assert_eq!(get("arca.policy.reason")["stringValue"], "no prod");
        assert_eq!(
            v["resourceSpans"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "arca-test"
        );
        // error outcome marks the span
        let ev = AuditEvent::op("a", arca_core::Op::List, None, "dev", Outcome::Error);
        let v = trace_payload(&cfg, &[ev]);
        assert_eq!(
            v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["status"]["code"],
            "STATUS_CODE_ERROR"
        );
    }
}
