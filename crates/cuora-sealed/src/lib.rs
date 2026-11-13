//! cuora-sealed — the sealed-pack contract, shared by the driver
//! verbs and the engine's `cuora://<pack>/<secret>` ref fallback.
//!
//! Tamias carries age-encrypted payloads **opaquely**; this crate
//! is the decrypt/seal side both front-ends use:
//!
//! - pack resolution against the tamias install store
//!   (`$TAMIAS_HOME/store` → XDG → `~/.local/share/tamias/store`
//!   → `./.tamias/store`, `--store`/`store` param overrides);
//! - the age identity keyfile (`FileIdentity` —
//!   `~/.config/cuora/identity.txt`, `$CUORA_IDENTITY_FILE`
//!   override, `--keyfile` override);
//! - `encrypt`/`decrypt` over `Zeroizing` buffers — plaintext
//!   exists only between read and write, never logged;
//! - [`open_pack_ref`] / [`pack_declares`] — the engine's read
//!   path: `cuora://<pack>/<secret>` resolves to a decrypted
//!   value (or a declared-secret probe for `cuora check`).
//!
//! The argv verbs (`seal`/`open`/`materialize`/`recipients`/
//! `packs`) live in `cuora-driver`'s `sealed` module and call
//! into this crate; `cuora-ops` calls [`open_pack_ref`] when a
//! vault lookup misses on a two-segment ref. Audit emission stays
//! with the callers (each knows its `Op` and actor).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::x25519;
use cuora_core::Ref;
use cuora_identity::IdentitySource;
use cuora_identity_file::FileIdentity;
use tamias_core::{Manifest, Sealed, StoreRecord, RECORD_FILE};
use zeroize::Zeroizing;

// ------------------------------------------------------------------ store

/// The tamias install store root — mirrors `tamias-cli`'s
/// `store_root()` minus the config-file location (callers pass
/// `--store`; `TAMIAS_HOME`/XDG cover the rest).
pub fn store_root(store: Option<&Path>) -> PathBuf {
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
#[derive(Debug)]
pub struct Installed {
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

    /// The payload dir — holds `tamias.yaml` + `sealed/`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `name@version` label for messages.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Bare pack name (`record.name`).
    pub fn name(&self) -> &str {
        &self.record.name
    }

    /// The store record (source ref, resolved digest, …).
    pub fn record(&self) -> &StoreRecord {
        &self.record
    }

    pub fn manifest(&self) -> Result<Manifest, String> {
        Manifest::load(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))
    }
}

/// Why [`find_pack`] found nothing usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindPackError {
    /// No installed entry matches the spec.
    #[error("sealed pack '{spec}' is not installed under {root}")]
    NotInstalled {
        /// The spec that was looked up.
        spec: String,
        /// The store root searched.
        root: PathBuf,
    },
    /// Several same-name installs — pick one by label.
    #[error("sealed pack '{spec}' has several installs — pick one: {candidates}")]
    Ambiguous {
        /// The spec that was looked up.
        spec: String,
        /// Candidate labels, comma-joined.
        candidates: String,
    },
}

/// Depth-first walk for dirs carrying `.tamias-store.json` — the
/// same shape `tamias-store-local`'s `collect` uses (dot-dirs and
/// symlinked records skipped). Unparsable records warn + skip.
pub fn scan_store(root: &Path) -> Vec<Installed> {
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
/// installs are [`FindPackError::Ambiguous`] (mirrors
/// `Store::find`'s ambiguity rule).
pub fn find_pack(root: &Path, spec: &str) -> Result<Installed, FindPackError> {
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
        0 => Err(FindPackError::NotInstalled {
            spec: spec.to_string(),
            root: root.to_path_buf(),
        }),
        1 => Ok(clone_installed(matches.swap_remove(0))),
        _ => Err(FindPackError::Ambiguous {
            spec: spec.to_string(),
            candidates: matches
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }),
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
#[derive(Debug)]
pub struct Target {
    /// Human label for messages (`name@version`, `--name`, or the
    /// `--dir` path).
    pub label: String,
    /// Bare pack name for audit refs — `record.name` from the store,
    /// `--name`, or the payload dir's file name.
    pub pack: String,
    /// Payload root (the dir holding `tamias.yaml` + `sealed/`).
    pub dir: PathBuf,
    /// The manifest's `sealed:` block.
    pub sealed: Sealed,
}

/// Resolve a pack spec against the store (or `--dir` mode: the
/// payload dir is handed over directly — driver dispatch). The
/// manifest must carry a `sealed:` block.
pub fn resolve_target(
    dir: Option<&Path>,
    name: Option<&str>,
    store: Option<&Path>,
    pack_spec: Option<&str>,
) -> Result<Target, String> {
    if let Some(dir) = dir {
        let manifest = Manifest::load(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let sealed = manifest
            .sealed
            .clone()
            .ok_or_else(|| format!("{} carries no sealed: block", dir.display()))?;
        let label = name
            .map(str::to_string)
            .unwrap_or_else(|| dir.display().to_string());
        let pack = name
            .map(str::to_string)
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "payload".into());
        return Ok(Target {
            label,
            pack,
            dir: dir.to_path_buf(),
            sealed,
        });
    }
    let spec =
        pack_spec.ok_or_else(|| "a pack name (or --dir <payload>) is required".to_string())?;
    let root = store_root(store);
    let entry = find_pack(&root, spec).map_err(|e| e.to_string())?;
    let manifest = entry.manifest()?;
    let sealed = manifest
        .sealed
        .clone()
        .ok_or_else(|| format!("pack '{spec}' carries no sealed: block"))?;
    Ok(Target {
        label: entry.label.clone(),
        pack: entry.record.name.clone(),
        dir: entry.dir.clone(),
        sealed,
    })
}

/// Parse `cuora://<pack>/<secret>` — the sealed-pack ref form. The
/// ref grammar allows deeper paths, but a sealed logical name is a
/// single `sealed.files` key, so exactly two segments.
pub fn parse_pack_ref(arg: &str) -> Result<(String, String), String> {
    let r = Ref::new(arg).map_err(|e| e.to_string())?;
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
pub fn identity_source(keyfile: Option<&Path>) -> FileIdentity {
    match keyfile {
        Some(p) => FileIdentity::new(p.to_path_buf()),
        None => FileIdentity::default(),
    }
}

pub fn load_identity(keyfile: Option<&Path>) -> Result<x25519::Identity, String> {
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

/// The acting identity for audit attribution — `CUORA_ACTOR`,
/// `$USER`, else `anon`. Callers that already resolved an actor
/// should prefer theirs.
pub fn actor() -> String {
    std::env::var("CUORA_ACTOR")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "anon".into())
}

// ------------------------------------------------------------------ age io

/// Encrypt `plain` to every recipient — fresh file key + nonce per
/// call (age guarantees; re-sealing never reproduces ciphertext).
pub fn encrypt(plain: &[u8], recipients: &[x25519::Recipient]) -> Result<Vec<u8>, String> {
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
pub fn decrypt(identity: &x25519::Identity, cipher: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
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
pub fn parse_recipient_lines(text: &str) -> Result<Vec<String>, String> {
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
pub fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

// ------------------------------------------------------- engine fallback

/// The result of [`open_pack_ref`] — a three-way answer so the
/// engine can tell "no pack answers this" (fall back to its usual
/// not-stored) from "the pack answered".
#[derive(Debug)]
pub enum PackOpen {
    /// No installed sealed pack named `pack` — or the match
    /// carries no `sealed:` block. The caller reports its normal
    /// miss.
    Miss,
    /// The pack is installed but declares no secret `secret`.
    NoSecret,
    /// Decrypted plaintext (`Zeroizing` — scrubbed on drop).
    Hit(Zeroizing<Vec<u8>>),
}

/// Resolve `cuora://<pack>/<secret>` against the tamias install
/// store and decrypt — the engine's pack-ref read path.
///
/// `store`/`keyfile` are the same overrides the driver verbs take
/// (`--store`/`--keyfile`); `None` means the standard resolution
/// (`TAMIAS_HOME`/XDG/`~/.local/share`, `CUORA_IDENTITY_FILE`/default
/// keyfile).
///
/// `Ok(Miss)`/`Ok(NoSecret)` are *answers*, not errors: the engine
/// maps them onto its not-stored reporting. `Err` is a real
/// failure — ambiguous installs, unreadable manifest/store record,
/// missing identity, decrypt failure — and should surface to the
/// user rather than silently degrade to a vault miss.
pub fn open_pack_ref(
    pack: &str,
    secret: &str,
    store: Option<&Path>,
    keyfile: Option<&Path>,
) -> Result<PackOpen, String> {
    let Some((target, sealed)) = find_sealed(pack, store)? else {
        return Ok(PackOpen::Miss);
    };
    let Some(rel) = sealed.files.get(secret) else {
        return Ok(PackOpen::NoSecret);
    };
    let cipher_path = target.dir.join(rel);
    let cipher =
        std::fs::read(&cipher_path).map_err(|e| format!("read {}: {e}", cipher_path.display()))?;
    let identity = load_identity(keyfile)?;
    let plain = decrypt(&identity, &cipher)?;
    Ok(PackOpen::Hit(plain))
}

/// Does an installed sealed pack `pack` declare `secret`? The
/// `cuora check` probe — no identity needed, no decrypt.
/// `Ok(false)` covers both "no such pack" and "pack doesn't
/// declare it"; `Err` is a real lookup failure (ambiguous install,
/// unreadable manifest).
pub fn pack_declares(pack: &str, secret: &str, store: Option<&Path>) -> Result<bool, String> {
    let Some((_, sealed)) = find_sealed(pack, store)? else {
        return Ok(false);
    };
    Ok(sealed.files.contains_key(secret))
}

/// Find the installed sealed pack `pack` spec resolves to —
/// `Ok(None)` when nothing matches or the match isn't sealed.
fn find_sealed(pack: &str, store: Option<&Path>) -> Result<Option<(Target, Sealed)>, String> {
    let root = store_root(store);
    let entry = match find_pack(&root, pack) {
        Ok(e) => e,
        Err(FindPackError::NotInstalled { .. }) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let manifest = entry.manifest()?;
    let Some(sealed) = manifest.sealed.clone() else {
        return Ok(None);
    };
    Ok(Some((
        Target {
            label: entry.label.clone(),
            pack: entry.record.name.clone(),
            dir: entry.dir.clone(),
            sealed: sealed.clone(),
        },
        sealed,
    )))
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
        assert_eq!(found[0].label(), "secrets-9aia@1.0.0");
        assert_eq!(found[0].name(), "secrets-9aia");
    }

    #[test]
    fn find_pack_errors_are_typed() {
        let root = tempfile::tempdir().unwrap();
        match find_pack(root.path(), "nope") {
            Err(FindPackError::NotInstalled { spec, .. }) => assert_eq!(spec, "nope"),
            other => panic!("expected NotInstalled, got {other:?}"),
        }
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

    /// Build a fake store entry: `<root>/<label>/` with a store
    /// record + `tamias.yaml` carrying a `sealed:` block + the
    /// encrypted payload, all keyed to `keyfile`'s identity.
    /// Returns nothing — asserts inside on setup failure.
    fn fake_store(root: &Path, label: &str, name: &str, secret: &str, value: &[u8], kf: &Path) {
        let (id, pubkey) = identity();
        // The keyfile the caller passes must BE this identity —
        // write it here so callers don't duplicate the format.
        let body = {
            use age::secrecy::ExposeSecret;
            format!(
                "# cuora identity\n# public: {pubkey}\n{}\n",
                id.to_string().expose_secret()
            )
        };
        std::fs::write(kf, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(kf, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let recip = x25519::Recipient::from_str(&pubkey).unwrap();
        let cipher = encrypt(value, &[recip]).unwrap();

        let dir = root.join(label);
        std::fs::create_dir_all(dir.join("sealed")).unwrap();
        std::fs::write(dir.join("sealed/recipients"), format!("{pubkey}\n")).unwrap();
        std::fs::write(dir.join(format!("sealed/{secret}.age")), &cipher).unwrap();
        std::fs::write(
            dir.join("tamias.yaml"),
            format!(
                "name: {name}\nversion: \"1.0.0\"\ntype: secrets\nsealed:\n  format: age/v1\n  recipients: sealed/recipients\n  files:\n    {secret}: sealed/{secret}.age\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(RECORD_FILE),
            format!(
                r#"{{"name":"{name}","version":"1.0.0","source_ref":"x","resolved":"sha256:abc","digest":"d","installed_at":0}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn open_pack_ref_hits_misses_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let kf = dir.path().join("identity.txt");
        fake_store(
            &store,
            "secrets-test@1.0.0",
            "secrets-test",
            "token",
            b"s3cr3t",
            &kf,
        );

        // Hit — decrypted value comes back zeroized.
        match open_pack_ref("secrets-test", "token", Some(&store), Some(&kf)).unwrap() {
            PackOpen::Hit(v) => assert_eq!(&*v, b"s3cr3t"),
            other => panic!("expected Hit, got {}", variant(&other)),
        }
        // Unknown pack → Miss (engine reports its usual not-stored).
        assert!(matches!(
            open_pack_ref("nope", "token", Some(&store), Some(&kf)).unwrap(),
            PackOpen::Miss
        ));
        // Pack installed, secret undeclared → NoSecret.
        assert!(matches!(
            open_pack_ref("secrets-test", "other", Some(&store), Some(&kf)).unwrap(),
            PackOpen::NoSecret
        ));
        // pack_declares agrees without touching identity/decrypt.
        assert!(pack_declares("secrets-test", "token", Some(&store)).unwrap());
        assert!(!pack_declares("secrets-test", "other", Some(&store)).unwrap());
        assert!(!pack_declares("nope", "token", Some(&store)).unwrap());
    }

    #[test]
    fn open_pack_ref_surfaces_real_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let kf = dir.path().join("identity.txt");
        fake_store(
            &store,
            "secrets-test@1.0.0",
            "secrets-test",
            "token",
            b"s3cr3t",
            &kf,
        );

        // Wrong keyfile → decrypt error surfaces (not a silent miss).
        let (other, _) = identity();
        let other_kf = dir.path().join("other.txt");
        {
            use age::secrecy::ExposeSecret;
            std::fs::write(
                &other_kf,
                format!(
                    "# cuora identity\n# public: {}\n{}\n",
                    other.to_public(),
                    other.to_string().expose_secret()
                ),
            )
            .unwrap();
        }
        assert!(open_pack_ref("secrets-test", "token", Some(&store), Some(&other_kf)).is_err());
        // Missing keyfile → identity error surfaces.
        assert!(open_pack_ref(
            "secrets-test",
            "token",
            Some(&store),
            Some(&dir.path().join("absent.txt"))
        )
        .is_err());
    }

    fn variant(o: &PackOpen) -> &'static str {
        match o {
            PackOpen::Miss => "Miss",
            PackOpen::NoSecret => "NoSecret",
            PackOpen::Hit(_) => "Hit",
        }
    }
}
