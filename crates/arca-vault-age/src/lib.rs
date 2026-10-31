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
        set_private(path)?;
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

    /// Encrypt + atomically rewrite the vault file.
    fn save(&self, payload: &Payload) -> Result<(), VaultError> {
        let encryptor = age::Encryptor::with_recipients(
            self.recipients.iter().map(|r| r as &dyn age::Recipient),
        )
        .map_err(|e| VaultError::Crypto(format!("encryptor: {e}")))?;
        let plain = serde_json::to_vec(payload)
            .map_err(|e| VaultError::Backend(format!("encode vault: {e}")))?;
        let mut cipher = Vec::new();
        let mut w = encryptor
            .wrap_output(&mut cipher)
            .map_err(|e| VaultError::Crypto(format!("encrypt vault: {e}")))?;
        w.write_all(&plain)?;
        w.finish()
            .map_err(|e| VaultError::Crypto(format!("encrypt vault: {e}")))?;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // tmp + rename so a crash can't leave a truncated vault.
        let tmp = self.path.with_extension("age.tmp");
        std::fs::write(&tmp, &cipher)?;
        set_private(&tmp)?;
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
}
