//! Sealed packs — tamias ADR-0004 / org P-027 / PL-017 task 3.
//!
//! Tamias carries age-encrypted payloads **opaquely**: the engine,
//! store, cache, registry, and logs digest ciphertext and never see
//! plaintext. This module is the argv-verb side — the verbs a
//! `type: secrets` (or any `sealed:`-carrying) pack's driver answers:
//!
//! - `seal` — author-side: plaintext files → `sealed/` payload
//!   layout (`sealed/recipients` + `sealed/<name>.age`), plus the
//!   `sealed:` manifest block on stdout — or merged straight into a
//!   `tamias.yaml` with `--manifest <path>` (surgical text merge:
//!   comments and unrelated keys survive; the result is validated
//!   as a manifest before it lands).
//! - `open <pack> <name>` / `open cuora://<pack>/<name>` — decrypt
//!   one logical secret to stdout or `--out` (0600). The escape
//!   hatch, like `cuora get`. `<pack>` may pin a version —
//!   `cuora://<pack>@<version>/<name>` or `<pack@version> <name>` —
//!   the tamias `name@version` label spelling, which is how
//!   multi-install ambiguity gets resolved.
//! - `materialize <pack> --out <dir>` — decrypt every declared
//!   secret into a fresh dir (0600 files).
//! - `recipients <pack>` — list the pack's age recipients (public
//!   keys) and mark which the local keyfile can open.
//! - `packs` — list installed sealed packs in the tamias store.
//! - `install`/`deploy`/`uninstall` — lifecycle no-ops: a sealed
//!   pack needs no driver action at install time (tamias dispatches
//!   them because `capabilities.types` claims `secrets`).
//!
//! All pack/store/identity/age machinery lives in `cuora-sealed` —
//! the same crate `cuora-ops` uses for the engine's
//! `cuora://<pack>/<secret>` ref fallback, so `cuora get`/`run`/
//! `env` and these verbs share one resolution + decrypt path.
//!
//! **Secrets safety:** plaintext exists only inside decrypt buffers
//! (`Zeroizing`) between read and write; nothing here logs values —
//! reports carry names, paths, and counts only. Every successful
//! open/materialize emits a `cuora` audit event (`Op::Get`) so the
//! materialization trail survives outside the vault path too.
//!
//! **Identity:** opening needs an age private key — the same
//! keyfile `cuora init` manages (`~/.config/cuora/identity.txt`,
//! mode 0600, `$CUORA_IDENTITY_FILE` override). `--keyfile` points
//! at another file in the same format. There is no passphrase
//! prompt: sealed packs are recipient-key only.
//!
//! **Store resolution:** `<pack>` resolves against the tamias
//! install store — `$TAMIAS_HOME/store`, `$XDG_DATA_HOME/tamias/
//! store`, `~/.local/share/tamias/store`, `./.tamias/store`
//! (`--store` overrides). `--dir <payload>` (what `tamias driver
//! exec`/install dispatch passes) skips the store entirely.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::x25519;
use cuora_core::Op;
use cuora_ops::{audit, AuditEvent, Outcome};
use cuora_sealed::{self, Target};
use serde_json::json;
use tamias_core::{is_valid_name, Manifest, Sealed};

/// Flags every sealed verb understands.
#[derive(Default)]
struct Flags {
    /// `--out <path>` (`--out=…` too).
    out: Option<PathBuf>,
    /// `--in <dir>` — seal's plaintext source dir.
    input_dir: Option<PathBuf>,
    /// `--dir <payload>` — payload dir directly (driver dispatch).
    dir: Option<PathBuf>,
    /// `--store <dir>` — tamias store root override.
    store: Option<PathBuf>,
    /// `--keyfile <path>` — age identity keyfile override.
    keyfile: Option<PathBuf>,
    /// `--name <label>` — informational (dispatch passes the pkg name).
    name: Option<String>,
    /// `--recipient <age1…>` — repeatable public recipients.
    recipients: Vec<String>,
    /// `--recipients <file>` — repeatable recipients files.
    recipient_files: Vec<PathBuf>,
    /// `--manifest <path>` — merge the printed `sealed:` block into
    /// this tamias.yaml instead of leaving it paste-by-hand.
    manifest: Option<PathBuf>,
}

/// Parse `--flag value` / `--flag=value`; unknown `--x` is an error,
/// everything else is positional.
fn parse_flags(args: &[String], known: &[&str]) -> Result<(Flags, Vec<String>), String> {
    let mut f = Flags::default();
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (flag, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ if a.starts_with("--") => (a.as_str(), None),
            _ => {
                pos.push(a.clone());
                i += 1;
                continue;
            }
        };
        if !known.contains(&flag) {
            return Err(format!("unknown flag {flag}"));
        }
        let value = match inline {
            Some(v) => v,
            None => {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))?
            }
        };
        match flag {
            "--out" => f.out = Some(PathBuf::from(value)),
            "--in" => f.input_dir = Some(PathBuf::from(value)),
            "--dir" => f.dir = Some(PathBuf::from(value)),
            "--store" => f.store = Some(PathBuf::from(value)),
            "--keyfile" => f.keyfile = Some(PathBuf::from(value)),
            "--name" => f.name = Some(value),
            "--recipient" => f.recipients.push(value),
            "--recipients" => f.recipient_files.push(PathBuf::from(value)),
            "--manifest" => f.manifest = Some(PathBuf::from(value)),
            _ => unreachable!("flag checked above"),
        }
        i += 1;
    }
    Ok((f, pos))
}

/// `--dir` mode: the payload dir is handed to us (driver dispatch).
/// Otherwise resolve `<pack>` (+ optional secret) from the store.
fn resolve_target(
    f: &Flags,
    pack_spec: Option<&str>,
    positional_secret: Option<&str>,
) -> Result<(Target, Option<String>), String> {
    let target = cuora_sealed::resolve_target(
        f.dir.as_deref(),
        f.name.as_deref(),
        f.store.as_deref(),
        pack_spec,
    )?;
    Ok((target, positional_secret.map(str::to_string)))
}

// ------------------------------------------------------------------ audit

/// One audit event per materialized secret — `Op::Get` against a
/// `cuora://<pack>/<name>` ref, env `sealed` (packs aren't
/// env-routed; the tag keeps the stream honest).
fn audit_open(target: &Target, name: &str, outcome: Outcome) {
    let rref = format!("cuora://{}/{name}", target.pack);
    audit::emit(&AuditEvent::op(
        &cuora_sealed::actor(),
        Op::Get,
        Some(&rref),
        "sealed",
        outcome,
    ));
    audit::flush();
}

// -------------------------------------------------------------------- seal

const SEAL_FLAGS: &[&str] = &[
    "--out",
    "--in",
    "--dir",
    "--store",
    "--keyfile",
    "--name",
    "--recipient",
    "--recipients",
    "--manifest",
];

/// `seal` — plaintext files → `sealed/` payload + the manifest block.
fn seal(args: &[String]) -> i32 {
    match seal_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cuora seal: {e}");
            1
        }
    }
}

fn seal_inner(args: &[String]) -> Result<i32, String> {
    let (f, pos) = parse_flags(args, SEAL_FLAGS)?;
    let out = f.out.clone().ok_or("seal needs --out <payload-dir>")?;

    // Collect (logical name, plaintext path) pairs.
    let mut secrets: Vec<(String, PathBuf)> = Vec::new();
    if let Some(dir) = &f.input_dir {
        let read = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|e| format!("stat {}: {e}", path.display()))?;
            if ft.is_dir() {
                return Err(format!(
                    "{} is a directory — seal takes files; nested layouts use NAME=PATH pairs",
                    path.display()
                ));
            }
            if !ft.is_file() {
                continue; // symlinks/fifos — not sealable payload
            }
            let fname = entry.file_name().to_string_lossy().into_owned();
            // Logical name = file stem (`token.txt` → `token`);
            // a bare `token` keeps its name.
            let stem = Path::new(&fname)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(fname);
            secrets.push((stem, path));
        }
        secrets.sort_by(|a, b| a.0.cmp(&b.0));
    }
    for p in &pos {
        let Some((name, path)) = p.split_once('=') else {
            return Err(format!(
                "seal operands are NAME=PATH pairs (got '{p}') — or use --in <dir>"
            ));
        };
        secrets.push((name.to_string(), PathBuf::from(path)));
    }
    if secrets.is_empty() {
        return Err("seal needs at least one secret — --in <dir> or NAME=PATH pairs".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, path) in &secrets {
        if !is_valid_name(name) {
            return Err(format!("invalid sealed secret name '{name}'"));
        }
        if !seen.insert(name.as_str()) {
            return Err(format!("duplicate sealed secret name '{name}'"));
        }
        if !path.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
    }

    // Recipients: --recipient args, --recipients files, --keyfile pub.
    let mut recipients: Vec<String> = f.recipients.clone();
    for rf in &f.recipient_files {
        let text =
            std::fs::read_to_string(rf).map_err(|e| format!("read {}: {e}", rf.display()))?;
        recipients.extend(cuora_sealed::parse_recipient_lines(&text)?);
    }
    if let Some(kf) = &f.keyfile {
        let id = cuora_sealed::load_identity(Some(kf))?;
        recipients.push(id.to_public().to_string());
    }
    recipients.sort();
    recipients.dedup();
    if recipients.is_empty() {
        return Err(
            "seal needs at least one recipient — --recipient age1…, --recipients <file>, or --keyfile"
                .into(),
        );
    }
    let recips: Vec<x25519::Recipient> = recipients
        .iter()
        .map(|r| {
            x25519::Recipient::from_str(r).map_err(|e| format!("invalid recipient {r:?}: {e}"))
        })
        .collect::<Result<_, _>>()?;

    // Encrypt + lay out `sealed/` inside --out.
    let sealed_dir = out.join("sealed");
    std::fs::create_dir_all(&sealed_dir)
        .map_err(|e| format!("create {}: {e}", sealed_dir.display()))?;
    let mut files = BTreeMap::new();
    for (name, path) in &secrets {
        let plain = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let cipher = cuora_sealed::encrypt(&plain, &recips)?;
        let rel = format!("sealed/{name}.age");
        let dest = out.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&dest, &cipher).map_err(|e| format!("write {}: {e}", dest.display()))?;
        files.insert(name.clone(), rel);
    }
    let recipients_body = format!("{}\n", recipients.join("\n"));
    std::fs::write(sealed_dir.join("recipients"), recipients_body)
        .map_err(|e| format!("write recipients: {e}"))?;

    // The manifest block — stdout is the machine-usable half.
    let mut block =
        String::from("sealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n");
    for (name, rel) in &files {
        block.push_str(&format!("    {name}: {rel}\n"));
    }
    print!("{block}");
    if let Some(manifest) = &f.manifest {
        merge_sealed_block(manifest, &block)?;
        eprintln!("merged the sealed: block into {}", manifest.display());
    } else if out.join("tamias.yaml").exists() {
        eprintln!(
            "hint: {} exists — re-run with --manifest to merge the block",
            out.join("tamias.yaml").display()
        );
    }
    eprintln!(
        "sealed {} secret(s) → {} ({} recipient(s)) — merge the block above into tamias.yaml, then `tamias pack`",
        secrets.len(),
        out.display(),
        recipients.len()
    );
    Ok(0)
}

// ------------------------------------------------------- manifest merge

/// Merge the generated `sealed:` block into an existing
/// `tamias.yaml`, preserving everything outside the block — comments,
/// key order, quoting, unrelated sections all survive byte-for-byte.
///
/// This is a *surgical text* merge, not a YAML round-trip: serde_yaml
/// would drop comments and reflow the file, which is exactly what an
/// author hand-merging wants to avoid. The trade-off is that block
/// detection is line-oriented — a top-level `sealed:` key and the
/// indented lines belonging to it. Anything weirder (a `sealed:` key
/// hidden inside a literal block scalar is impossible at column 0;
/// a column-0 comment *inside* the sealed mapping truncates the
/// replaced region) either merges correctly or fails the post-merge
/// [`Manifest::load_at`] validation with the file left untouched.
///
/// The merge is validated before it lands: the result is written to
/// a sibling temp file, parsed as a tamias manifest, and only then
/// renamed over the original (permissions carried over).
fn merge_sealed_block(path: &Path, block: &str) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    // The target must already be a valid package manifest — merging
    // into a project/deps-only tamias.yaml is out of scope.
    Manifest::load_at(path).map_err(|e| {
        format!(
            "{} is not a valid tamias package manifest ({e}) — --manifest merges into a pack's tamias.yaml",
            path.display()
        )
    })?;
    let merged = merge_sealed_text(&text, block);
    commit_manifest(path, &merged)
}

/// The pure half of [`merge_sealed_block`]: splice `block` (a
/// `sealed:\n…` YAML fragment) into `text` at the top-level `sealed:`
/// key, replacing its old block, or appending at EOF when absent.
fn merge_sealed_text(text: &str, block: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    // Locate the top-level `sealed:` key — column 0, `key:` followed
    // by end-of-line, whitespace, or a comment.
    let key_at = lines.iter().position(|l| {
        l.strip_prefix("sealed:").is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(' ')
                || rest.starts_with('\t')
                || rest.starts_with('#')
        })
    });
    let block_lines: Vec<&str> = block.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + block_lines.len() + 2);
    match key_at {
        Some(i) => {
            out.extend_from_slice(&lines[..i]);
            out.extend(block_lines.iter().copied());
            // The old block runs while lines stay inside the mapping:
            // blank or indented. A column-0 line (next key, comment,
            // `---`/`...`) ends it — a column-0 comment *inside* the
            // mapping truncates the splice and the post-merge
            // validation reports it rather than eating the comment.
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.trim().is_empty() || l.starts_with(' ') || l.starts_with('\t') {
                    j += 1;
                } else {
                    break;
                }
            }
            out.extend_from_slice(&lines[j..]);
        }
        None => {
            out.extend_from_slice(&lines);
            // Separate the appended block from a non-empty tail.
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push("");
            }
            out.extend(block_lines.iter().copied());
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Validate `merged` as a tamias manifest, then rename it over
/// `path`. The temp file keeps the `.yaml` extension so
/// [`Manifest::load_at`] picks the YAML parser, and inherits the
/// original's permissions before the rename.
fn commit_manifest(path: &Path, merged: &str) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no usable file name", path.display()))?;
    let tmp = path.with_file_name(format!("{file_name}.cuora-merge.yaml"));
    let write = |p: &Path| -> Result<(), String> {
        std::fs::write(p, merged).map_err(|e| format!("write {}: {e}", p.display()))
    };
    write(&tmp)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    let result = Manifest::load_at(&tmp).map_err(|e| {
        format!(
            "merged manifest failed validation ({e}) — {} left untouched",
            path.display()
        )
    });
    match result {
        Ok(_) => {
            std::fs::rename(&tmp, path)
                .map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

// ------------------------------------------------------- open/materialize

const OPEN_FLAGS: &[&str] = &["--out", "--dir", "--store", "--keyfile", "--name"];
const MATERIALIZE_FLAGS: &[&str] = &["--out", "--dir", "--store", "--keyfile", "--name"];
const RECIPIENTS_FLAGS: &[&str] = &["--dir", "--store", "--keyfile", "--name"];
const PACKS_FLAGS: &[&str] = &["--store"];

/// `open` — decrypt one logical secret. `open <pack> <name>`,
/// `open cuora://<pack>/<name>`, or dispatch form
/// `open --dir <payload> --name <pkg> <name>`.
fn open(args: &[String]) -> i32 {
    match open_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cuora open: {e}");
            1
        }
    }
}

fn open_inner(args: &[String]) -> Result<i32, String> {
    let (f, pos) = parse_flags(args, OPEN_FLAGS)?;
    // Forms: `cuora://p/s` alone; `<pack> <name>`; `--dir` + `<name>`.
    let (pack_spec, secret) = if f.dir.is_some() {
        // Dispatch form — the payload dir is explicit, so exactly
        // one positional: the secret name.
        match pos.as_slice() {
            [name] => (String::new(), name.clone()),
            _ => return Err("open with --dir takes exactly one operand: the secret name".into()),
        }
    } else if pos.len() == 1 {
        if pos[0].starts_with("cuora://") || pos[0].starts_with("arca://") {
            cuora_sealed::parse_pack_ref(&pos[0])?
        } else {
            return Err(format!(
                "open needs <pack> <name> or cuora://<pack>/<name> (got '{}')",
                pos[0]
            ));
        }
    } else if pos.len() == 2 {
        (pos[0].clone(), pos[1].clone())
    } else {
        return Err(
            "open needs <pack> <name>, cuora://<pack>/<name>, or --dir <payload> <name>".into(),
        );
    };
    let pack_spec = if pack_spec.is_empty() {
        None
    } else {
        Some(pack_spec.as_str())
    };
    let (target, secret) = resolve_target(&f, pack_spec, Some(&secret))?;
    let secret = secret.ok_or("open needs a secret name")?;

    let rel = target
        .sealed
        .files
        .get(&secret)
        .ok_or_else(|| format!("'{}' declares no sealed secret '{secret}'", target.label))?;
    let cipher_path = target.dir.join(rel);
    let cipher =
        std::fs::read(&cipher_path).map_err(|e| format!("read {}: {e}", cipher_path.display()))?;
    let identity = cuora_sealed::load_identity(f.keyfile.as_deref())?;
    let plain = cuora_sealed::decrypt(&identity, &cipher)?;

    match &f.out {
        Some(out) => {
            cuora_sealed::write_secret_file(out, &plain)?;
            eprintln!("opened {secret} → {}", out.display());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&plain)
                .map_err(|e| format!("write stdout: {e}"))?;
            if !plain.ends_with(b"\n") {
                let _ = stdout.write_all(b"\n");
            }
        }
    }
    audit_open(&target, &secret, Outcome::Ok);
    Ok(0)
}

/// `materialize` — decrypt every declared secret into `--out`.
fn materialize(args: &[String]) -> i32 {
    match materialize_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cuora materialize: {e}");
            1
        }
    }
}

fn materialize_inner(args: &[String]) -> Result<i32, String> {
    let (f, pos) = parse_flags(args, MATERIALIZE_FLAGS)?;
    let out = f.out.clone().ok_or("materialize needs --out <dir>")?;
    let pack_spec = match pos.as_slice() {
        [] if f.dir.is_some() => None,
        [p] if f.dir.is_none() => Some(p.as_str()),
        _ => return Err(
            "materialize takes one pack spec (<pack>, @scope/name@ver, or --dir with no operands)"
                .into(),
        ),
    };
    // Refuse to write into a non-empty dir — materialization is
    // all-or-nothing over a fresh target.
    if out.exists() {
        let mut it = std::fs::read_dir(&out).map_err(|e| format!("read {}: {e}", out.display()))?;
        if it.next().is_some() {
            return Err(format!(
                "{} exists and is not empty — materialize wants a fresh dir",
                out.display()
            ));
        }
    }
    let (target, _) = resolve_target(&f, pack_spec, None)?;
    let identity = cuora_sealed::load_identity(f.keyfile.as_deref())?;

    let mut wrote = 0usize;
    for (name, rel) in &target.sealed.files {
        let cipher_path = target.dir.join(rel);
        let cipher = std::fs::read(&cipher_path)
            .map_err(|e| format!("read {}: {e}", cipher_path.display()))?;
        let plain = cuora_sealed::decrypt(&identity, &cipher)?;
        // Logical names are manifest-validated ([A-Za-z0-9._-]) —
        // flat files, no traversal possible.
        cuora_sealed::write_secret_file(&out.join(name), &plain)?;
        audit_open(&target, name, Outcome::Ok);
        wrote += 1;
    }
    eprintln!("materialized {wrote} secret(s) → {}", out.display());
    Ok(0)
}

/// `recipients` — the pack's public recipient set + local match.
fn recipients(args: &[String]) -> i32 {
    match recipients_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cuora recipients: {e}");
            1
        }
    }
}

fn recipients_inner(args: &[String]) -> Result<i32, String> {
    let (f, pos) = parse_flags(args, RECIPIENTS_FLAGS)?;
    let pack_spec = match pos.as_slice() {
        [] if f.dir.is_some() => None,
        [p] if f.dir.is_none() => Some(p.as_str()),
        _ => return Err("recipients takes one pack spec (or --dir with no operands)".into()),
    };
    let (target, _) = resolve_target(&f, pack_spec, None)?;
    let recipients_path = target.dir.join(&target.sealed.recipients);
    let text = std::fs::read_to_string(&recipients_path)
        .map_err(|e| format!("read {}: {e}", recipients_path.display()))?;
    let list = cuora_sealed::parse_recipient_lines(&text)?;

    // Local match is best-effort: no keyfile → still list publics.
    let local = match cuora_sealed::load_identity(f.keyfile.as_deref()) {
        Ok(id) => Some(id.to_public().to_string()),
        Err(_) => None,
    };
    println!("pack:      {}", target.label);
    println!("recipients file: {}", target.sealed.recipients);
    for r in &list {
        let mark = if local.as_deref() == Some(r.as_str()) {
            "  ← local keyfile"
        } else {
            ""
        };
        println!("{r}{mark}");
    }
    if list.is_empty() {
        eprintln!("warning: recipients file holds no keys");
    }
    if local.is_none() {
        eprintln!("note: no local identity — cannot test which recipients this host opens");
    }
    Ok(0)
}

/// `packs` — installed sealed packs in the store (names only).
fn packs(args: &[String]) -> i32 {
    match packs_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cuora packs: {e}");
            1
        }
    }
}

fn packs_inner(args: &[String]) -> Result<i32, String> {
    let (f, pos) = parse_flags(args, PACKS_FLAGS)?;
    if !pos.is_empty() {
        return Err("packs takes no operands".into());
    }
    let root = cuora_sealed::store_root(f.store.as_deref());
    let mut found = 0usize;
    for e in cuora_sealed::scan_store(&root) {
        let Ok(m) = e.manifest() else { continue };
        let Some(sealed) = &m.sealed else { continue };
        let n = sealed.files.len();
        println!(
            "{}\t{}\t{} secret(s)\t{}",
            e.label(),
            m.name,
            n,
            e.dir().display()
        );
        found += 1;
    }
    if found == 0 {
        eprintln!("no sealed packs installed under {}", root.display());
    }
    Ok(0)
}

// ------------------------------------------------------------- lifecycle

/// `install`/`deploy`/`uninstall` — dispatched by tamias for
/// `type: secrets` packs (the driver's `capabilities.types` claims
/// the kind). Sealed packs need no lifecycle action: ciphertext
/// installs verbatim and opens consumer-side. Anything *not* sealed
/// fails loudly — this driver only owns the sealed contract.
fn lifecycle(verb: &str, args: &[String]) -> i32 {
    // Tamias appends `--dir <payload> --name <pkg> [--config …]`;
    // accept and ignore everything but --dir/--name.
    let (f, _) = match parse_flags(args, &["--dir", "--name", "--config"]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cuora {verb}: {e}");
            return 1;
        }
    };
    let Some(dir) = f.dir else {
        eprintln!("cuora {verb}: --dir <payload> is required (tamias dispatch provides it)");
        return 1;
    };
    match Manifest::load(&dir) {
        Ok(m) if m.sealed.is_some() => {
            eprintln!(
                "cuora {verb}: {} is a sealed pack — ciphertext only, nothing to {}",
                f.name.as_deref().unwrap_or("pack"),
                verb
            );
            0
        }
        Ok(_) => {
            eprintln!(
                "cuora {verb}: {} carries no sealed: block — the cuora driver only handles sealed packs",
                dir.display()
            );
            1
        }
        Err(e) => {
            eprintln!("cuora {verb}: {}", e);
            1
        }
    }
}

/// Entry point from `main` — `args` is everything after the verb.
pub fn run(verb: &str, args: &[String]) -> i32 {
    match verb {
        "seal" => seal(args),
        "open" => open(args),
        "materialize" => materialize(args),
        "recipients" => recipients(args),
        "packs" => packs(args),
        "install" | "deploy" | "uninstall" => lifecycle(verb, args),
        _ => {
            eprintln!("unknown sealed verb '{verb}'");
            2
        }
    }
}

/// The manifest `verbs`/`types` additions — sealed ops plus the
/// lifecycle verbs tamias dispatches for `type: secrets` packs.
pub fn verbs() -> &'static [&'static str] {
    &["seal", "open", "materialize", "recipients", "packs"]
}

/// The tamias content kinds this driver claims via
/// `capabilities.types` — `find_driver`'s capabilities fallback maps
/// `type: secrets` packs here.
pub fn types() -> &'static [&'static str] {
    &["secrets"]
}

/// The sealed block as JSON — for `--json`-style consumers and the
/// ops manifest's self-description.
#[allow(dead_code)]
pub fn sealed_json(sealed: &Sealed) -> serde_json::Value {
    json!({
        "format": sealed.format,
        "recipients": sealed.recipients,
        "files": sealed.files,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn identity() -> (x25519::Identity, String) {
        let id = x25519::Identity::generate();
        let public = id.to_public().to_string();
        (id, public)
    }

    #[test]
    fn flags_parse_both_forms() {
        let args: Vec<String> = [
            "--out=/tmp/x",
            "--recipient",
            "age1abc",
            "pos1",
            "--in",
            "/tmp/in",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (f, pos) = parse_flags(&args, SEAL_FLAGS).unwrap();
        assert_eq!(f.out.unwrap(), PathBuf::from("/tmp/x"));
        assert_eq!(f.recipients, vec!["age1abc"]);
        assert_eq!(pos, vec!["pos1".to_string()]);
        assert!(parse_flags(&["--bogus".into()], SEAL_FLAGS).is_err());
        assert!(parse_flags(&["--out".into()], SEAL_FLAGS).is_err());
    }

    #[test]
    fn seal_open_materialize_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("token.txt"), b"s3cr3t-value").unwrap();
        // keyfile in FileIdentity format (rage-compatible: secret
        // line + `# public:` comment)
        let (id, pubkey) = identity();
        let kf = dir.path().join("identity.txt");
        let body = {
            use age::secrecy::ExposeSecret;
            format!(
                "# cuora identity\n# public: {pubkey}\n{}\n",
                id.to_string().expose_secret()
            )
        };
        std::fs::write(&kf, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&kf, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let out = dir.path().join("payload");
        let args: Vec<String> = [
            "--in".to_string(),
            src.display().to_string(),
            "--out".to_string(),
            out.display().to_string(),
            "--keyfile".to_string(),
            kf.display().to_string(),
        ]
        .to_vec();
        assert_eq!(seal(&args), 0, "seal failed");
        assert!(out.join("sealed/token.age").is_file());
        assert!(out.join("sealed/recipients").is_file());
        // The author merges the printed block into tamias.yaml —
        // --dir resolution requires the manifest.
        std::fs::write(
            out.join("tamias.yaml"),
            "name: secrets-test\nversion: \"1.0.0\"\ntype: secrets\nsealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    token: sealed/token.age\n",
        )
        .unwrap();

        // --dir mode open
        let oargs: Vec<String> = [
            "--dir".to_string(),
            out.display().to_string(),
            "--keyfile".to_string(),
            kf.display().to_string(),
            "token".to_string(),
        ]
        .to_vec();
        // capture stdout? open prints value — assert exit 0 via inner pieces instead:
        // resolve_target + decrypt path is covered; run open for real:
        assert_eq!(open(&oargs), 0);

        let mdir = dir.path().join("materialized");
        let margs: Vec<String> = [
            "--dir".to_string(),
            out.display().to_string(),
            "--out".to_string(),
            mdir.display().to_string(),
            "--keyfile".to_string(),
            kf.display().to_string(),
        ]
        .to_vec();
        assert_eq!(materialize(&margs), 0);
        assert_eq!(std::fs::read(mdir.join("token")).unwrap(), b"s3cr3t-value");
    }

    #[test]
    fn lifecycle_requires_sealed_manifest() {
        let dir = tempfile::tempdir().unwrap();
        // no manifest at all → error
        let args: Vec<String> = vec!["--dir".into(), dir.path().display().to_string()];
        assert_eq!(lifecycle("install", &args), 1);
        // plaintext manifest → refused
        std::fs::write(dir.path().join("tamias.yaml"), "name: x\n").unwrap();
        assert_eq!(lifecycle("install", &args), 1);
        // sealed manifest → no-op success
        std::fs::write(
            dir.path().join("tamias.yaml"),
            "name: x\nsealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    t: sealed/t.age\n",
        )
        .unwrap();
        assert_eq!(lifecycle("install", &args), 0);
        assert_eq!(lifecycle("uninstall", &args), 0);
    }

    const BLOCK: &str = "sealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    token: sealed/token.age\n";

    #[test]
    fn manifest_merge_text_append_and_replace() {
        // Append: no sealed key — block lands at EOF, comments kept.
        let src = "# pack header\nname: x\nversion: \"1.0.0\"\ntype: secrets\n";
        let m = merge_sealed_text(src, BLOCK);
        assert!(m.starts_with("# pack header\nname: x"));
        assert!(m.contains("\ntype: secrets\n\nsealed:\n"));
        assert!(m.trim_end().ends_with("token: sealed/token.age"));

        // Replace: old block swapped, position + neighbors preserved.
        let src = "name: x\nversion: \"1.0.0\"\n# keep me\ntype: secrets\n\nsealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    old: sealed/old.age\n\nbins:\n  - name: b\n";
        let m = merge_sealed_text(src, BLOCK);
        assert!(m.contains("# keep me"));
        assert!(m.contains("bins:\n  - name: b\n"));
        assert!(!m.contains("old: sealed/old.age"));
        assert!(m.contains("token: sealed/token.age"));
        assert!(m.find("sealed:").unwrap() < m.find("bins:").unwrap());

        // Inline `sealed: {}` is a one-line block — replaced whole.
        let m = merge_sealed_text("name: x\nversion: \"1.0.0\"\nsealed: {}\n", BLOCK);
        assert!(!m.contains("sealed: {}"));
        assert!(m.contains("files:\n    token: sealed/token.age"));

        // `sealed:` with a trailing comment still counts as the key.
        let m = merge_sealed_text(
            "name: x\nversion: \"1.0.0\"\nsealed: # ciphertext only\n  files: {}\n",
            BLOCK,
        );
        assert!(m.contains("format: age/v1"));
        assert!(!m.contains("ciphertext only"));
    }

    #[test]
    fn manifest_merge_block_validates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("tamias.yaml");
        std::fs::write(
            &manifest,
            "# authored pack\nname: merge-test\nversion: \"1.0.0\"\ntype: secrets\n\nsealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    stale: sealed/stale.age\n",
        )
        .unwrap();
        merge_sealed_block(&manifest, BLOCK).unwrap();
        let after = std::fs::read_to_string(&manifest).unwrap();
        assert!(after.starts_with("# authored pack\nname: merge-test"));
        assert!(after.contains("token: sealed/token.age"));
        assert!(!after.contains("stale:"));
        // temp merge file is gone
        assert!(!dir.path().join("tamias.yaml.cuora-merge.yaml").exists());
        // and the result still loads as a manifest
        Manifest::load_at(&manifest).unwrap();

        // A non-manifest target fails before touching the file.
        let bogus = dir.path().join("bogus.yaml");
        std::fs::write(&bogus, "just: a\nrandom: doc\n").unwrap();
        let before = std::fs::read_to_string(&bogus).unwrap();
        assert!(merge_sealed_block(&bogus, BLOCK).is_err());
        assert_eq!(std::fs::read_to_string(&bogus).unwrap(), before);
        assert!(!dir.path().join("bogus.yaml.cuora-merge.yaml").exists());
    }

    #[test]
    fn sealed_json_shape() {
        let s = Sealed {
            format: tamias_core::AGE_FORMAT.into(),
            recipients: "sealed/recipients".into(),
            files: BTreeMap::from([("t".into(), "sealed/t.age".into())]),
        };
        let v = sealed_json(&s);
        assert_eq!(v["format"], tamias_core::AGE_FORMAT);
        assert_eq!(v["files"]["t"], "sealed/t.age");
    }
}
