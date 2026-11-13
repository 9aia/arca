//! Sealed packs — tamias ADR-0004 / org P-027 / PL-017 task 3.
//!
//! Tamias carries age-encrypted payloads **opaquely**: the engine,
//! store, cache, registry, and logs digest ciphertext and never see
//! plaintext. This module is the decrypt/seal side — the verbs a
//! `type: secrets` (or any `sealed:`-carrying) pack's driver answers:
//!
//! - `seal` — author-side: plaintext files → `sealed/` payload
//!   layout (`sealed/recipients` + `sealed/<name>.age`), plus the
//!   `sealed:` manifest block to paste into `tamias.yaml`.
//! - `open <pack> <name>` / `open cuora://<pack>/<name>` — decrypt
//!   one logical secret to stdout or `--out` (0600). The escape
//!   hatch, like `cuora get`.
//! - `materialize <pack> --out <dir>` — decrypt every declared
//!   secret into a fresh dir (0600 files).
//! - `recipients <pack>` — list the pack's age recipients (public
//!   keys) and mark which the local keyfile can open.
//! - `packs` — list installed sealed packs in the tamias store.
//! - `install`/`deploy`/`uninstall` — lifecycle no-ops: a sealed
//!   pack needs no driver action at install time (tamias dispatches
//!   them because `capabilities.types` claims `secrets`).
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
use cuora_identity::IdentitySource;
use cuora_identity_file::FileIdentity;
use cuora_ops::{audit, AuditEvent, Outcome};
use serde_json::json;
use tamias_core::{is_valid_name, Manifest, Sealed, StoreRecord, RECORD_FILE};
use zeroize::Zeroizing;

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
            _ => unreachable!("flag checked above"),
        }
        i += 1;
    }
    Ok((f, pos))
}

// ------------------------------------------------------------------ store

/// The tamias install store root — mirrors `tamias-cli`'s
/// `store_root()` minus the config-file location (the driver has no
/// tamias config surface; `--store`/`TAMIAS_HOME`/XDG cover it).
fn store_root(store: Option<&Path>) -> PathBuf {
    if let Some(s) = store {
        return s.to_path_buf();
    }
    if let Some(root) = std::env::var_os("TAMIAS_HOME") {
        return PathBuf::from(root).join("store");
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(|d| PathBuf::from(d).join("tamias/store"))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/tamias/store"))
        })
        .unwrap_or_else(|| PathBuf::from(".tamias/store"))
}

/// One installed store entry — dir + record (manifest loads lazily).
struct Installed {
    dir: PathBuf,
    record: StoreRecord,
    label: String,
}

impl Installed {
    /// `name@version` / `@scope/name@version` — same spelling
    /// `tamias-store-local`'s `StoreEntry::label` produces.
    fn label_of(record: &StoreRecord) -> String {
        let scoped = match &record.scope {
            Some(scope) => format!("@{scope}/{}", record.name),
            None => record.name.clone(),
        };
        let version = record.version.clone().unwrap_or_else(|| {
            let sha = record
                .resolved
                .strip_prefix("sha256:")
                .unwrap_or(&record.resolved);
            sha.chars().take(12).collect()
        });
        format!("{scoped}@{version}")
    }

    fn manifest(&self) -> Result<Manifest, String> {
        Manifest::load(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))
    }
}

/// Depth-first walk for dirs carrying `.tamias-store.json` — the
/// same shape `tamias-store-local`'s `collect` uses (dot-dirs and
/// symlinked records skipped). Unparsable records warn + skip.
fn scan_store(root: &Path) -> Vec<Installed> {
    let mut out = Vec::new();
    collect_entries(root, &mut out);
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn collect_entries(dir: &Path, out: &mut Vec<Installed>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        // `file_type` never follows symlinks — a linked dir isn't
        // an entry (same rule as the store's own walk).
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let record_path = path.join(RECORD_FILE);
        let is_file = std::fs::symlink_metadata(&record_path)
            .map(|m| m.is_file())
            .unwrap_or(false);
        if is_file {
            match std::fs::read_to_string(&record_path)
                .ok()
                .and_then(|t| serde_json::from_str::<StoreRecord>(&t).ok())
            {
                Some(record) => {
                    let label = Installed::label_of(&record);
                    out.push(Installed {
                        dir: path,
                        record,
                        label,
                    });
                }
                None => eprintln!(
                    "cuora sealed: skipping unparsable record {}",
                    record_path.display()
                ),
            }
        } else {
            collect_entries(&path, out);
        }
    }
}

/// Resolve a pack spec — bare name, `@scope/name`, `name@version`,
/// or a full `label()` — to an installed entry. Several same-name
/// installs are an error listing candidates (mirrors
/// `Store::find`'s ambiguity rule).
fn find_pack(root: &Path, spec: &str) -> Result<Installed, String> {
    let entries = scan_store(root);
    // Exact label first (`@scope/name@1.0.0`, `name@1.0.0`).
    if let Some(e) = entries.iter().find(|e| e.label == spec) {
        return Ok(clone_installed(e));
    }
    let scoped = |e: &Installed| match &e.record.scope {
        Some(s) => format!("@{s}/{}", e.record.name),
        None => e.record.name.clone(),
    };
    let mut matches: Vec<&Installed> = entries
        .iter()
        .filter(|e| e.record.name == spec || scoped(e) == spec || e.label == spec)
        .collect();
    match matches.len() {
        0 => Err(format!(
            "sealed pack '{spec}' is not installed under {}",
            root.display()
        )),
        1 => Ok(clone_installed(matches.swap_remove(0))),
        _ => Err(format!(
            "sealed pack '{spec}' has several installs — pick one: {}",
            matches
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn clone_installed(e: &Installed) -> Installed {
    Installed {
        dir: e.dir.clone(),
        record: e.record.clone(),
        label: e.label.clone(),
    }
}

// ------------------------------------------------------------- resolution

/// What a verb operates on — a payload dir plus its manifest's
/// `sealed:` block.
struct Target {
    /// Human label for messages (`name@version`, `--name`, or the
    /// `--dir` path).
    label: String,
    /// Bare pack name for audit refs — `record.name` from the store,
    /// `--name`, or the payload dir's file name.
    pack: String,
    /// Payload root (the dir holding `tamias.yaml` + `sealed/`).
    dir: PathBuf,
    sealed: Sealed,
}

/// `--dir` mode: the payload dir is handed to us (driver dispatch).
/// Otherwise resolve `<pack>` (+ optional secret) from the store.
fn resolve_target(
    f: &Flags,
    pack_spec: Option<&str>,
    positional_secret: Option<&str>,
) -> Result<(Target, Option<String>), String> {
    if let Some(dir) = &f.dir {
        let manifest = Manifest::load(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let sealed = manifest
            .sealed
            .clone()
            .ok_or_else(|| format!("{} carries no sealed: block", dir.display()))?;
        let label = f.name.clone().unwrap_or_else(|| dir.display().to_string());
        let pack = f
            .name
            .clone()
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "payload".into());
        return Ok((
            Target {
                label,
                pack,
                dir: dir.clone(),
                sealed,
            },
            positional_secret.map(str::to_string),
        ));
    }
    let spec =
        pack_spec.ok_or_else(|| "a pack name (or --dir <payload>) is required".to_string())?;
    let entry = find_pack(&store_root(f.store.as_deref()), spec)?;
    let manifest = entry.manifest()?;
    let sealed = manifest
        .sealed
        .clone()
        .ok_or_else(|| format!("pack '{spec}' carries no sealed: block"))?;
    Ok((
        Target {
            label: entry.label.clone(),
            pack: entry.record.name.clone(),
            dir: entry.dir.clone(),
            sealed,
        },
        positional_secret.map(str::to_string),
    ))
}

/// Parse `cuora://<pack>/<secret>` — the sealed-pack ref form. The
/// ref grammar allows deeper paths, but a sealed logical name is a
/// single `sealed.files` key, so exactly two segments.
fn parse_pack_ref(arg: &str) -> Result<(String, String), String> {
    let r = cuora_core::Ref::new(arg).map_err(|e| e.to_string())?;
    let segs = r.segments();
    if segs.len() != 2 {
        return Err(format!(
            "sealed pack ref '{arg}' must be cuora://<pack>/<secret> (got {})",
            r.path()
        ));
    }
    Ok((segs[0].clone(), segs[1].clone()))
}

// ---------------------------------------------------------------- identity

/// The identity keyfile source — `--keyfile` or the default
/// (`$CUORA_IDENTITY_FILE` → `~/.config/cuora/identity.txt`, mode
/// 0600, legacy `arca` fallback — all inside `FileIdentity`).
fn identity_source(keyfile: Option<&Path>) -> FileIdentity {
    match keyfile {
        Some(p) => FileIdentity::new(p.to_path_buf()),
        None => FileIdentity::default(),
    }
}

fn load_identity(keyfile: Option<&Path>) -> Result<x25519::Identity, String> {
    let src = identity_source(keyfile);
    src.load()
        .map_err(|e| format!("identity from {}: {e}", src.describe()))?
        .ok_or_else(|| {
            format!(
                "no age identity at {} — run `cuora init --with-file` or set CUORA_IDENTITY_FILE",
                src.path().display()
            )
        })
}

// ------------------------------------------------------------------ age io

/// Encrypt `plain` to every recipient — fresh file key + nonce per
/// call (age guarantees; re-sealing never reproduces ciphertext).
fn encrypt(plain: &[u8], recipients: &[x25519::Recipient]) -> Result<Vec<u8>, String> {
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))
            .map_err(|e| format!("age encryptor: {e}"))?;
    let mut cipher = Vec::new();
    let mut w = encryptor
        .wrap_output(&mut cipher)
        .map_err(|e| format!("age wrap: {e}"))?;
    w.write_all(plain).map_err(|e| format!("age write: {e}"))?;
    w.finish().map_err(|e| format!("age finish: {e}"))?;
    Ok(cipher)
}

/// Decrypt an age file with `identity` — scrypt/passphrase files
/// are refused (sealed packs are recipient-key only, same rule as
/// the vault).
fn decrypt(identity: &x25519::Identity, cipher: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let decryptor =
        age::Decryptor::new_buffered(cipher).map_err(|e| format!("not an age file: {e}"))?;
    if decryptor.is_scrypt() {
        return Err("sealed payload is passphrase-encrypted; recipient key required".into());
    }
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| format!("decrypt: {e}"))?;
    let mut buf = Zeroizing::new(Vec::new());
    std::io::Read::read_to_end(&mut reader, &mut buf)
        .map_err(|e| format!("read plaintext: {e}"))?;
    Ok(buf)
}

/// Parse a recipients file — one `age1…` per line, `#` comments and
/// blanks skipped (the same shape `sealed/recipients` ships).
fn parse_recipient_lines(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        x25519::Recipient::from_str(line).map_err(|e| format!("recipient line {}: {e}", n + 1))?;
        out.push(line.to_string());
    }
    Ok(out)
}

/// Write `bytes` to `path` with parent dirs created; on unix the
/// file lands `0600` at open (no world-readable window) — materialized
/// secrets are as sensitive as the keyfile.
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))
}

// ------------------------------------------------------------------ audit

fn actor() -> String {
    std::env::var("CUORA_ACTOR")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "anon".into())
}

/// One audit event per materialized secret — `Op::Get` against a
/// `cuora://<pack>/<name>` ref, env `sealed` (packs aren't
/// env-routed; the tag keeps the stream honest).
fn audit_open(target: &Target, name: &str, outcome: Outcome) {
    let rref = format!("cuora://{}/{name}", target.pack);
    audit::emit(&AuditEvent::op(
        &actor(),
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
        recipients.extend(parse_recipient_lines(&text)?);
    }
    if let Some(kf) = &f.keyfile {
        let id = load_identity(Some(kf))?;
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
        let cipher = encrypt(&plain, &recips)?;
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
    eprintln!(
        "sealed {} secret(s) → {} ({} recipient(s)) — merge the block above into tamias.yaml, then `tamias pack`",
        secrets.len(),
        out.display(),
        recipients.len()
    );
    Ok(0)
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
            parse_pack_ref(&pos[0])?
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
    let identity = load_identity(f.keyfile.as_deref())?;
    let plain = decrypt(&identity, &cipher)?;

    match &f.out {
        Some(out) => {
            write_secret_file(out, &plain)?;
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
    let identity = load_identity(f.keyfile.as_deref())?;

    let mut wrote = 0usize;
    for (name, rel) in &target.sealed.files {
        let cipher_path = target.dir.join(rel);
        let cipher = std::fs::read(&cipher_path)
            .map_err(|e| format!("read {}: {e}", cipher_path.display()))?;
        let plain = decrypt(&identity, &cipher)?;
        // Logical names are manifest-validated ([A-Za-z0-9._-]) —
        // flat files, no traversal possible.
        write_secret_file(&out.join(name), &plain)?;
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
    let list = parse_recipient_lines(&text)?;

    // Local match is best-effort: no keyfile → still list publics.
    let local = match load_identity(f.keyfile.as_deref()) {
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
    let root = store_root(f.store.as_deref());
    let mut found = 0usize;
    for e in scan_store(&root) {
        let Ok(m) = e.manifest() else { continue };
        let Some(sealed) = &m.sealed else { continue };
        let n = sealed.files.len();
        println!(
            "{}\t{}\t{} secret(s)\t{}",
            e.label,
            m.name,
            n,
            e.dir.display()
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
    fn encrypt_decrypt_roundtrip() {
        let (id, pubkey) = identity();
        let recip = x25519::Recipient::from_str(&pubkey).unwrap();
        let cipher = encrypt(b"hunter2", &[recip]).unwrap();
        assert!(cipher.starts_with(tamias_core::AGE_MAGIC));
        let plain = decrypt(&id, &cipher).unwrap();
        assert_eq!(&*plain, b"hunter2");
    }

    #[test]
    fn decrypt_rejects_wrong_key_and_scrypt_shape() {
        let (id, pubkey) = identity();
        let (other, _) = identity();
        let recip = x25519::Recipient::from_str(&pubkey).unwrap();
        let cipher = encrypt(b"x", &[recip]).unwrap();
        assert!(decrypt(&other, &cipher).is_err());
        assert!(decrypt(&id, b"not age").is_err());
    }

    #[test]
    fn recipient_lines_parse_comments_and_blanks() {
        let (_, pubkey) = identity();
        let text = format!("# comment\n\n  {pubkey}  \n");
        assert_eq!(parse_recipient_lines(&text).unwrap(), vec![pubkey]);
        assert!(parse_recipient_lines("age1bad").is_err());
    }

    #[test]
    fn pack_ref_requires_two_segments() {
        assert_eq!(
            parse_pack_ref("cuora://secrets-9aia/db-dsn").unwrap(),
            ("secrets-9aia".into(), "db-dsn".into())
        );
        assert!(parse_pack_ref("cuora://onlypack").is_err());
        assert!(parse_pack_ref("cuora://a/b/c").is_err());
        assert!(parse_pack_ref("billing/stripe").is_err());
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
    fn scan_store_finds_entries_and_skips_junk() {
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join("secrets-9aia@1.0.0");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join(RECORD_FILE),
            r#"{"name":"secrets-9aia","version":"1.0.0","source_ref":"x","resolved":"sha256:abc","digest":"d","installed_at":0}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".hidden")).unwrap();
        std::fs::write(root.path().join("stray.txt"), "x").unwrap();
        let found = scan_store(root.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "secrets-9aia@1.0.0");
        assert_eq!(found[0].record.name, "secrets-9aia");
    }

    #[test]
    fn write_secret_file_lands_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/s.txt");
        write_secret_file(&path, b"v").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
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
