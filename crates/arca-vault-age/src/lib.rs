//! arca-vault-age — the local vault, and the default backend
//! forever (per PL-001 hard constraints).
//!
//! One age-x25519-encrypted JSON doc holds every env's records;
//! `put`/`rm`/`rotate` rewrite the whole file (fine at v1 scale).
//! Decrypted payload shape (`v` is the format version):
//!
//! ```json
//! {
//!   "v": 1,
//!   "envs": {
//!     "dev": {
//!       "billing/stripe": {
//!         "value": "…", "version": 1,
//!         "created_at": "…", "rotated_at": "…"
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Identity is an age keyfile (one `AGE-SECRET-KEY-…` line,
//! `#` comments allowed) at `~/.config/arca/identity.txt`;
//! recipients default to that identity's public key plus any extra
//! `recipients` in the vault's config. [`AgeVault::ensure_identity`]
//! generates a fresh keyfile (mode `0600`) when missing — that path
//! backs `arca init`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use age::x25519;
use arca_core::{Env, Ref, Secret};
use arca_vault::{SecretRecord, Vault, VaultError};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Current payload format version.
const FORMAT_VERSION: u32 = 1;

/// The decrypted vault document.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Payload {
    v: u32,
    #[serde(default)]
    envs: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// One stored secret, as persisted (the plaintext lives only inside
/// the ciphertext — this struct is the *decrypted* in-memory form).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    value: String,
    #[serde(default = "one")]
    version: u64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    rotated_at: Option<OffsetDateTime>,
}

fn one() -> u64 {
    1
}

impl Entry {
    fn record(&self) -> SecretRecord {
        SecretRecord {
            value: Secret::new(self.value.clone()),
            version: self.version,
            created_at: self.created_at,
            rotated_at: self.rotated_at,
        }
    }
}

/// The age-encrypted local vault.
pub struct AgeVault {
    path: PathBuf,
    identity: x25519::Identity,
    recipients: Vec<x25519::Recipient>,
    state: Mutex<Payload>,
}

impl AgeVault {
    /// The default vault file — `$XDG_DATA_HOME/arca/vault.age`,
    /// else `~/.local/share/arca/vault.age`.
    pub fn default_vault_path() -> PathBuf {
        data_home().join("arca/vault.age")
    }

    /// The default identity keyfile — `$ARCA_IDENTITY_FILE`, else
    /// `$XDG_CONFIG_HOME/arca/identity.txt`, else
    /// `~/.config/arca/identity.txt`.
    pub fn default_identity_path() -> PathBuf {
        if let Ok(p) = std::env::var("ARCA_IDENTITY_FILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        config_home().join("arca/identity.txt")
    }

    /// Read + validate an age identity keyfile: first non-empty,
    /// non-`#`-comment line must be an `AGE-SECRET-KEY-…` string.
    pub fn read_identity(path: &Path) -> Result<x25519::Identity, VaultError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| VaultError::Config(format!("read identity {}: {e}", path.display())))?;
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| {
                VaultError::Config(format!("identity {} holds no key", path.display()))
            })?;
        x25519::Identity::from_str(line)
            .map_err(|e| VaultError::Crypto(format!("identity {}: {e}", path.display())))
    }

    /// Create the identity keyfile (mode `0600`, parents created)
    /// iff missing; returns `true` when one was generated — the
    /// `arca init` path. The public recipient goes in a `#` comment
    /// line so the file stays rage-compatible.
    #[allow(clippy::unwrap_used)]
    pub fn ensure_identity(path: &Path) -> Result<bool, VaultError> {
        if path.exists() {
            return Ok(false);
        }
        use age::secrecy::ExposeSecret;
        let identity = x25519::Identity::generate();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = identity.to_string();
        let body = format!(
            "# arca identity — created {}\n# public: {}\n{}\n",
            OffsetDateTime::now_utc(),
            identity.to_public(),
            key.expose_secret(),
        );
        std::fs::write(path, body)?;
        // chmod'ing a file this process just created and owns
        // cannot realistically fail — an expect is honest here
        // where a `?` would imply a reachable error arm.
        set_private(path).expect("chmod on a freshly written file");
        Ok(true)
    }

    /// Open the vault. `identity` must exist (see
    /// [`Self::ensure_identity`] for bootstrap); a missing vault file
    /// is an empty vault. `extra_recipients` are `age1…` strings
    /// encrypted to alongside the identity's public key.
    pub fn open(
        vault_path: &Path,
        identity_path: &Path,
        extra_recipients: &[String],
    ) -> Result<Self, VaultError> {
        let identity = Self::read_identity(identity_path)?;
        let mut recipients = vec![identity.to_public()];
        for r in extra_recipients {
            recipients.push(
                x25519::Recipient::from_str(r)
                    .map_err(|e| VaultError::Config(format!("invalid recipient {r:?}: {e}")))?,
            );
        }
        let payload = match std::fs::read(vault_path) {
            Ok(bytes) => Self::decrypt(&identity, &bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Payload::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(AgeVault {
            path: vault_path.to_path_buf(),
            identity,
            recipients,
            state: Mutex::new(payload),
        })
    }

    /// [`Self::ensure_identity`] + [`Self::open`] — bootstrap when
    /// the keyfile is missing, then open. Returns the vault and
    /// whether a fresh identity was generated.
    pub fn open_or_init(
        vault_path: &Path,
        identity_path: &Path,
        extra_recipients: &[String],
    ) -> Result<(Self, bool), VaultError> {
        let created = Self::ensure_identity(identity_path)?;
        Ok((
            Self::open(vault_path, identity_path, extra_recipients)?,
            created,
        ))
    }

    /// The identity's public recipient (`age1…`).
    pub fn public_recipient(&self) -> String {
        self.identity.to_public().to_string()
    }

    fn decrypt(identity: &x25519::Identity, bytes: &[u8]) -> Result<Payload, VaultError> {
        let decryptor = age::Decryptor::new_buffered(bytes)
            .map_err(|e| VaultError::Crypto(format!("read vault: {e}")))?;
        if decryptor.is_scrypt() {
            return Err(VaultError::Crypto(
                "vault is passphrase-encrypted; recipient key required".into(),
            ));
        }
        let mut reader = decryptor
            .decrypt(std::iter::once(identity as &dyn age::Identity))
            .map_err(|e| VaultError::Crypto(format!("decrypt vault: {e}")))?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        let payload: Payload = serde_json::from_slice(&buf)
            .map_err(|e| VaultError::Crypto(format!("vault payload: {e}")))?;
        if payload.v > FORMAT_VERSION {
            return Err(VaultError::Crypto(format!(
                "vault format v{} is newer than supported v{FORMAT_VERSION}",
                payload.v
            )));
        }
        Ok(payload)
    }

    /// Serialize + encrypt a payload to all recipients. In-memory
    /// encryption cannot realistically fail — `recipients` is never
    /// empty (the identity's public key is always first) and a
    /// `Vec` sink never errors — so the `expect`s document
    /// invariants rather than guard reachable paths.
    #[allow(clippy::unwrap_used)]
    fn encrypt_payload(&self, payload: &Payload) -> Vec<u8> {
        let encryptor = age::Encryptor::with_recipients(
            self.recipients.iter().map(|r| r as &dyn age::Recipient),
        )
        .expect("at least one recipient");
        let plain = serde_json::to_vec(payload).expect("payload serializes");
        let mut cipher = Vec::new();
        {
            let mut w = encryptor.wrap_output(&mut cipher).expect("in-memory wrap");
            w.write_all(&plain).expect("in-memory write");
            w.finish().expect("in-memory finish");
        }
        cipher
    }

    /// Encrypt + atomically rewrite the vault file.
    #[allow(clippy::unwrap_used)]
    fn save(&self, payload: &Payload) -> Result<(), VaultError> {
        let cipher = self.encrypt_payload(payload);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // tmp + rename so a crash can't leave a truncated vault.
        let tmp = self.path.with_extension("age.tmp");
        std::fs::write(&tmp, &cipher)?;
        set_private(&tmp).expect("chmod on a freshly written file");
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Lock the payload; poison = unrecoverable backend error.
    fn state(&self) -> Result<std::sync::MutexGuard<'_, Payload>, VaultError> {
        self.state
            .lock()
            .map_err(|_| VaultError::Backend("vault state lock poisoned".into()))
    }
}

impl Vault for AgeVault {
    fn get(&self, env: &Env, rref: &Ref) -> Result<Option<SecretRecord>, VaultError> {
        let p = self.state()?;
        Ok(p.envs
            .get(env.as_str())
            .and_then(|m| m.get(&rref.path()))
            .map(|e| e.record()))
    }

    fn put(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        let mut p = self.state()?;
        let map = p.envs.entry(env.as_str().to_string()).or_default();
        let entry = match map.get_mut(&rref.path()) {
            Some(e) => {
                e.value = value.expose().to_string();
                e.version += 1;
                e.clone()
            }
            None => {
                let e = Entry {
                    value: value.expose().to_string(),
                    version: 1,
                    created_at: OffsetDateTime::now_utc(),
                    rotated_at: None,
                };
                map.insert(rref.path(), e.clone());
                e
            }
        };
        self.save(&p)?;
        Ok(entry.record())
    }

    fn rm(&self, env: &Env, rref: &Ref) -> Result<bool, VaultError> {
        let mut p = self.state()?;
        let map = match p.envs.get_mut(env.as_str()) {
            Some(m) => m,
            None => return Ok(false),
        };
        if map.remove(&rref.path()).is_none() {
            return Ok(false);
        }
        self.save(&p)?;
        Ok(true)
    }

    fn list(&self, env: &Env) -> Result<Vec<Ref>, VaultError> {
        let p = self.state()?;
        let mut out = Vec::new();
        if let Some(map) = p.envs.get(env.as_str()) {
            for path in map.keys() {
                out.push(
                    Ref::from_path(path)
                        .map_err(|e| VaultError::Backend(format!("stored key {e}")))?,
                );
            }
        }
        out.sort();
        Ok(out)
    }

    fn rotate(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        let mut p = self.state()?;
        let map = p.envs.entry(env.as_str().to_string()).or_default();
        let e = map
            .get_mut(&rref.path())
            .ok_or_else(|| VaultError::NotFound {
                rref: rref.path(),
                env: env.as_str().to_string(),
            })?;
        e.value = value.expose().to_string();
        e.version += 1;
        e.rotated_at = Some(OffsetDateTime::now_utc());
        let entry = e.clone();
        self.save(&p)?;
        Ok(entry.record())
    }
}

/// `~` for path defaults.
fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/share"))
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"))
}

/// `chmod 0600` — identity files hold key material; the vault file
/// is ciphertext but private-by-default is still right.
#[cfg(unix)]
fn set_private(path: &Path) -> Result<(), VaultError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private(_path: &Path) -> Result<(), VaultError> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ensure_identity_generates_0600_keyfile() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        assert!(AgeVault::ensure_identity(&id_path).unwrap());
        assert!(!AgeVault::ensure_identity(&id_path).unwrap());
        let id = AgeVault::read_identity(&id_path).unwrap();
        let text = std::fs::read_to_string(&id_path).unwrap();
        assert!(text.contains(&id.to_public().to_string()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&id_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn read_identity_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        std::fs::write(&id_path, "# only comments\n\n").unwrap();
        assert!(AgeVault::read_identity(&id_path).is_err());
        std::fs::write(&id_path, "not-a-key\n").unwrap();
        assert!(AgeVault::read_identity(&id_path).is_err());
    }

    #[test]
    fn ciphertext_hides_values() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        let v_path = dir.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        let fake = "fake-value-not-real";
        v.put(
            &Env::dev(),
            &Ref::new("arca://x/y").unwrap(),
            Secret::new(fake),
        )
        .unwrap();
        let bytes = std::fs::read(&v_path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(fake));
        assert!(!text.contains("billing"));
    }

    #[test]
    fn recipient_file_comments_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        use age::secrecy::ExposeSecret;
        let id = x25519::Identity::generate();
        let mut f = std::fs::File::create(&id_path).unwrap();
        writeln!(f, "# comment first").unwrap();
        writeln!(f, "{}", id.to_string().expose_secret()).unwrap();
        assert_eq!(
            AgeVault::read_identity(&id_path).unwrap().to_public(),
            id.to_public()
        );
    }

    /// Age-encrypt `data` to a single recipient — fixtures for the
    /// decrypt/open failure modes below.
    fn encrypt_to(recipient: &x25519::Recipient, data: &[u8]) -> Vec<u8> {
        let encryptor =
            age::Encryptor::with_recipients([recipient as &dyn age::Recipient].into_iter())
                .unwrap();
        let mut cipher = Vec::new();
        {
            let mut w = encryptor.wrap_output(&mut cipher).unwrap();
            w.write_all(data).unwrap();
            w.finish().unwrap();
        }
        cipher
    }

    #[test]
    fn ensure_identity_surfaces_write_failures() {
        let dir = tempfile::tempdir().unwrap();
        // a file where a parent dir must be → create_dir_all fails
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        assert!(AgeVault::ensure_identity(&blocker.join("sub/id.txt")).is_err());
        // an empty path has no parent and no writable name
        assert!(AgeVault::ensure_identity(Path::new("")).is_err());
        // missing parents get created on demand
        let nested = dir.path().join("a/b/identity.txt");
        assert!(AgeVault::ensure_identity(&nested).unwrap());
        assert!(nested.exists());
    }

    #[test]
    fn set_private_on_missing_file_is_an_error() {
        assert!(set_private(Path::new("/nonexistent/x/y")).is_err());
    }

    #[test]
    fn default_paths_follow_xdg_then_home() {
        // ARCA_IDENTITY_FILE wins outright
        temp_env::with_var("ARCA_IDENTITY_FILE", Some("/tmp/arca-id-override"), || {
            assert_eq!(
                AgeVault::default_identity_path(),
                PathBuf::from("/tmp/arca-id-override")
            );
        });
        // empty override is ignored; XDG vars drive the rest
        temp_env::with_vars(
            [
                ("ARCA_IDENTITY_FILE", Some("")),
                ("XDG_CONFIG_HOME", Some("/tmp/xdg-c")),
                ("XDG_DATA_HOME", Some("/tmp/xdg-d")),
                ("HOME", Some("/tmp/fakehome")),
            ],
            || {
                assert_eq!(
                    AgeVault::default_identity_path(),
                    PathBuf::from("/tmp/xdg-c/arca/identity.txt")
                );
                assert_eq!(
                    AgeVault::default_vault_path(),
                    PathBuf::from("/tmp/xdg-d/arca/vault.age")
                );
            },
        );
        // no XDG → HOME-based fallback
        temp_env::with_vars(
            [
                ("ARCA_IDENTITY_FILE", None::<&str>),
                ("XDG_CONFIG_HOME", None::<&str>),
                ("XDG_DATA_HOME", None::<&str>),
                ("HOME", Some("/tmp/fakehome")),
            ],
            || {
                assert_eq!(
                    AgeVault::default_vault_path(),
                    PathBuf::from("/tmp/fakehome/.local/share/arca/vault.age")
                );
                assert_eq!(
                    AgeVault::default_identity_path(),
                    PathBuf::from("/tmp/fakehome/.config/arca/identity.txt")
                );
            },
        );
        // no HOME either → "."
        temp_env::with_vars(
            [
                ("ARCA_IDENTITY_FILE", None::<&str>),
                ("XDG_CONFIG_HOME", None::<&str>),
                ("XDG_DATA_HOME", None::<&str>),
                ("HOME", None::<&str>),
            ],
            || {
                assert_eq!(
                    AgeVault::default_vault_path(),
                    PathBuf::from("./.local/share/arca/vault.age")
                );
            },
        );
    }

    #[test]
    fn open_validates_recipients_and_vault_reads() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        let v_path = dir.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        // a valid extra recipient opens and reports the identity key
        let extra = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, &id_path, &[extra.to_public().to_string()]).unwrap();
        let me = AgeVault::read_identity(&id_path).unwrap();
        assert_eq!(v.public_recipient(), me.to_public().to_string());
        // an invalid recipient string is a config error
        assert!(AgeVault::open(&v_path, &id_path, &["not-a-recipient".into()]).is_err());
        // a directory where the vault file should be → i/o error
        let as_dir = dir.path().join("vaultdir");
        std::fs::create_dir(&as_dir).unwrap();
        assert!(AgeVault::open(&as_dir, &id_path, &[]).is_err());
        // garbage where ciphertext should be → crypto error
        let garbage = dir.path().join("garbage.age");
        std::fs::write(&garbage, b"definitely not age").unwrap();
        assert!(AgeVault::open(&garbage, &id_path, &[]).is_err());
        // missing identity → config error up front
        assert!(AgeVault::open(&v_path, &dir.path().join("no-id.txt"), &[]).is_err());
    }

    #[test]
    fn open_or_init_bootstraps_then_opens() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        let v_path = dir.path().join("vault.age");
        let (_v, created) = AgeVault::open_or_init(&v_path, &id_path, &[]).unwrap();
        assert!(created);
        let (_v, created) = AgeVault::open_or_init(&v_path, &id_path, &[]).unwrap();
        assert!(!created);
        // identity can't be ensured → error
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        assert!(AgeVault::open_or_init(&v_path, &blocker.join("id.txt"), &[]).is_err());
        // identity exists but is garbage → open fails
        let bad = dir.path().join("bad.txt");
        std::fs::write(&bad, "junk\n").unwrap();
        assert!(AgeVault::open_or_init(&v_path, &bad, &[]).is_err());
    }

    #[test]
    fn decrypt_reports_every_failure_mode() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        AgeVault::ensure_identity(&id_path).unwrap();
        let id = AgeVault::read_identity(&id_path).unwrap();
        // not an age file at all
        assert!(AgeVault::decrypt(&id, b"garbage").is_err());
        // passphrase-encrypted → we hold no passphrase
        let scrypt = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
            "fake-pw".to_string(),
        ));
        let mut blob = Vec::new();
        {
            let mut w = scrypt.wrap_output(&mut blob).unwrap();
            w.write_all(b"{}").unwrap();
            w.finish().unwrap();
        }
        let e = AgeVault::decrypt(&id, &blob).unwrap_err();
        assert!(e.to_string().contains("passphrase"), "{e}");
        // encrypted to someone else → can't decrypt
        let other = x25519::Identity::generate();
        assert!(AgeVault::decrypt(&id, &encrypt_to(&other.to_public(), b"{}")).is_err());
        // valid envelope, garbage plaintext → payload error
        let blob = encrypt_to(&id.to_public(), b"not json");
        assert!(AgeVault::decrypt(&id, &blob).is_err());
        // a newer format version is refused, honestly
        let blob = encrypt_to(&id.to_public(), br#"{"v":99,"envs":{}}"#);
        let e = AgeVault::decrypt(&id, &blob).unwrap_err();
        assert!(e.to_string().contains("v99"), "{e}");
        // truncated mid-ciphertext → the stream read fails
        let good = encrypt_to(&id.to_public(), br#"{"v":1,"envs":{}}"#);
        assert!(AgeVault::decrypt(&id, &good[..good.len() / 2]).is_err());
    }

    #[test]
    fn entries_without_version_fields_default_to_one() {
        // a payload entry lacking "version"/"rotated_at" picks up
        // serde defaults — older writes still read back sanely
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        let v_path = dir.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let id = AgeVault::read_identity(&id_path).unwrap();
        let plain = br#"{"v":1,"envs":{"dev":{"x/y":{"value":"fake-old","created_at":"2024-01-01T00:00:00Z"}}}}"#;
        std::fs::write(&v_path, encrypt_to(&id.to_public(), plain)).unwrap();
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        let rec = v
            .get(&Env::dev(), &Ref::new("arca://x/y").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(rec.version, 1);
        assert_eq!(rec.value.expose(), "fake-old");
        assert!(rec.rotated_at.is_none());
    }

    #[test]
    fn poisoned_state_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        let v_path = dir.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        // die holding the lock → the mutex is poisoned for good
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = v.state.lock().unwrap();
            panic!("intentional poison");
        }));
        let r = Ref::new("arca://x/y").unwrap();
        let e = Env::dev();
        // every verb reports the poisoned state as a backend error
        assert!(matches!(v.get(&e, &r), Err(VaultError::Backend(_))));
        assert!(matches!(
            v.put(&e, &r, Secret::new("f")),
            Err(VaultError::Backend(_))
        ));
        assert!(matches!(v.rm(&e, &r), Err(VaultError::Backend(_))));
        assert!(matches!(v.list(&e), Err(VaultError::Backend(_))));
        assert!(matches!(
            v.rotate(&e, &r, Secret::new("f")),
            Err(VaultError::Backend(_))
        ));
    }

    #[test]
    fn save_surfaces_filesystem_failures() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        AgeVault::ensure_identity(&id_path).unwrap();
        let r = Ref::new("arca://x/y").unwrap();

        // parent dir swapped for a file → create_dir_all can't proceed
        let parent = dir.path().join("swapped");
        let v_path = parent.join("vault.age");
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        std::fs::write(&parent, "not a dir").unwrap();
        assert!(v.put(&Env::dev(), &r, Secret::new("fake")).is_err());
        let _ = std::fs::remove_file(&parent);

        // vault path became a directory → the rename can't land
        let v_path = dir.path().join("isdir.age");
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        std::fs::create_dir(&v_path).unwrap();
        assert!(v.put(&Env::dev(), &r, Secret::new("fake")).is_err());

        // read-only vault dir → every mutating verb's save fails
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = dir.path().join("ro");
            std::fs::create_dir(&sub).unwrap();
            let v_path = sub.join("vault.age");
            let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
            v.put(&Env::dev(), &r, Secret::new("fake")).unwrap();
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
            assert!(v.put(&Env::dev(), &r, Secret::new("fake2")).is_err());
            assert!(v.rm(&Env::dev(), &r).is_err());
            assert!(v.rotate(&Env::dev(), &r, Secret::new("fake3")).is_err());
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn list_rejects_a_stored_key_that_isnt_a_ref() {
        // put validates refs, so only a hand-edited payload could
        // carry a bad key — list must error, never panic
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("id.txt");
        let v_path = dir.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let v = AgeVault::open(&v_path, &id_path, &[]).unwrap();
        {
            let mut p = v.state.lock().unwrap();
            p.envs.entry("dev".into()).or_default().insert(
                "bad key!".into(),
                Entry {
                    value: "fake".into(),
                    version: 1,
                    created_at: OffsetDateTime::now_utc(),
                    rotated_at: None,
                },
            );
        }
        assert!(v.list(&Env::dev()).is_err());
    }
}
