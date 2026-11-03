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
//! The vault takes an already-resolved [`x25519::Identity`] —
//! where the identity *lives* is the `arca-identity` port's
//! business (keyfile via `arca-identity-file`, session keyring via
//! `arca-identity-keyring`; `arca-ops` resolves it). Recipients
//! default to that identity's public key plus any extra
//! `recipients` in the vault's config.
//! [`AgeVault::open_or_init`] is the bootstrap path — it runs
//! [`arca_identity::ensure`] on the caller's chosen source.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use age::x25519;
use arca_core::{Env, Ref, Secret};
use arca_identity::IdentitySource;
use arca_vault::{SecretRecord, Vault, VaultError};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use zeroize::Zeroizing;

/// Current payload format version.
const FORMAT_VERSION: u32 = 1;

/// The decrypted vault document. **No `Debug`** — the whole point
/// of the payload is that it holds plaintext; a derived dump would
/// print every value in the vault.
#[derive(Default, Serialize, Deserialize)]
struct Payload {
    v: u32,
    #[serde(default)]
    envs: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// One stored secret, as persisted (the plaintext lives only inside
/// the ciphertext — this struct is the *decrypted* in-memory form).
/// `Zeroizing` scrubs the value buffer on drop; no `Debug` for the
/// same reason as [`Payload`].
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    value: Zeroizing<String>,
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

/// How long a mutating verb waits for a held vault lock before
/// failing — vault writes are milliseconds, so this is generous.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(2_000);
/// A lockfile older than this is reclaimed — its holder is
/// presumed dead (crash between create and remove).
const LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(120);

/// Cross-process exclusion for the vault's read-modify-write
/// cycle (MED-3). `flock`/`fcntl` aren't in std and a new
/// dependency for one syscall is heavier than this: `<vault>.lock`
/// is created with `create_new` (atomic `O_EXCL`), so exactly one
/// process holds it. A stale lock from a dead holder is reclaimed
/// after [`LOCK_STALE`]; `Drop` removes it.
struct VaultLock {
    path: PathBuf,
}

impl VaultLock {
    fn acquire(vault: &Path) -> Result<Self, VaultError> {
        let path = vault.with_extension("age.lock");
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    // pid + creation timestamp — provenance for a
                    // human debugging a stuck lock, and the
                    // staleness signal for the next acquirer
                    let _ = writeln!(f, "{} {}", std::process::id(), unix_ms());
                    return Ok(VaultLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::held_is_stale(&path) {
                        // best-effort: if another process wins the
                        // race to re-create, our next loop loses
                        // create_new anyway
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(VaultError::Backend(format!(
                            "vault is locked by another process ({})",
                            path.display()
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// A held lock is stale when the creation timestamp inside it
    /// is older than [`LOCK_STALE`] — an unreadable or
    /// foreign-shaped file is treated as held, never clobbered.
    fn held_is_stale(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|created| unix_ms().saturating_sub(created) > LOCK_STALE.as_millis() as u64)
            .unwrap_or(false)
    }
}

/// Milliseconds since the epoch — the lockfile's creation stamp.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Entry {
    fn record(&self) -> SecretRecord {
        SecretRecord {
            value: Secret::new(self.value.as_str()),
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

    /// Open the vault with an already-resolved identity — where the
    /// identity came from (keyfile, keyring) is the caller's
    /// business. A missing vault file is an empty vault.
    /// `extra_recipients` are `age1…` strings encrypted to alongside
    /// the identity's public key.
    pub fn open(
        vault_path: &Path,
        identity: x25519::Identity,
        extra_recipients: &[String],
    ) -> Result<Self, VaultError> {
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

    /// [`arca_identity::ensure`] + [`Self::open`] — bootstrap the
    /// identity on `source` when it holds none, then open. Returns
    /// the vault and whether a fresh identity was generated.
    pub fn open_or_init(
        vault_path: &Path,
        source: &dyn IdentitySource,
        extra_recipients: &[String],
    ) -> Result<(Self, bool), VaultError> {
        let (identity, created) = arca_identity::ensure(source)
            .map_err(|e| VaultError::Config(format!("identity from {}: {e}", source.describe())))?;
        Ok((Self::open(vault_path, identity, extra_recipients)?, created))
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
        // Zeroizing — the decrypted plaintext is scrubbed on drop
        // rather than left in a freed heap buffer (MED-4).
        let mut buf = Zeroizing::new(Vec::new());
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
        let plain = Zeroizing::new(serde_json::to_vec(payload).expect("payload serializes"));
        let mut cipher = Vec::new();
        {
            let mut w = encryptor.wrap_output(&mut cipher).expect("in-memory wrap");
            w.write_all(&plain).expect("in-memory write");
            w.finish().expect("in-memory finish");
        }
        cipher
    }

    /// Encrypt + atomically rewrite the vault file — tmp + fsync +
    /// rename + dir fsync, so a crash can't leave a truncated or
    /// unflushed vault (MED-3).
    #[allow(clippy::unwrap_used)]
    fn save(&self, payload: &Payload) -> Result<(), VaultError> {
        let cipher = self.encrypt_payload(payload);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("age.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&cipher)?;
            f.sync_all()?;
        }
        set_private(&tmp).expect("chmod on a freshly written file");
        std::fs::rename(&tmp, &self.path)?;
        sync_dir(self.path.parent());
        Ok(())
    }

    /// Lock the payload; poison = unrecoverable backend error.
    fn state(&self) -> Result<std::sync::MutexGuard<'_, Payload>, VaultError> {
        self.state
            .lock()
            .map_err(|_| VaultError::Backend("vault state lock poisoned".into()))
    }

    /// The on-disk payload — a missing file is an empty vault.
    /// Only [`Self::mutate`] re-reads (under the lock, before
    /// applying its change): reads keep the open-time cache —
    /// re-reading on every `get`/`list` would both stall on
    /// special files and cost a decrypt per verb (MED-3).
    fn read_disk(&self) -> Result<Payload, VaultError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Self::decrypt(&self.identity, &bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Payload::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// The mutating-verb spine (MED-3): hold the process mutex,
    /// take the cross-process [`VaultLock`], **re-read** the vault
    /// file so another process's completed write isn't clobbered,
    /// apply `f`, persist when `f` reports a change, and leave
    /// `state` mirroring the new on-disk payload.
    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut Payload) -> Result<(T, bool), VaultError>,
    ) -> Result<T, VaultError> {
        let mut p = self.state()?;
        // the lockfile lives next to the vault — the dir may not
        // exist yet on a first write (save creates it too)
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = VaultLock::acquire(&self.path)?;
        *p = self.read_disk()?;
        let (out, dirty) = f(&mut p)?;
        if dirty {
            self.save(&p)?;
        }
        Ok(out)
    }
}

/// Best-effort fsync of the vault's directory — the durability
/// hint for the rename. Failure isn't a vault failure.
#[cfg(unix)]
fn sync_dir(parent: Option<&Path>) {
    if let Some(d) = parent {
        if let Ok(f) = std::fs::File::open(d) {
            let _ = f.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_dir(_parent: Option<&Path>) {}

impl Vault for AgeVault {
    fn get(&self, env: &Env, rref: &Ref) -> Result<Option<SecretRecord>, VaultError> {
        let p = self.state()?;
        Ok(p.envs
            .get(env.as_str())
            .and_then(|m| m.get(&rref.path()))
            .map(|e| e.record()))
    }

    fn put(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        self.mutate(|p| {
            let map = p.envs.entry(env.as_str().to_string()).or_default();
            let entry = match map.get_mut(&rref.path()) {
                Some(e) => {
                    e.value = Zeroizing::new(value.expose().to_string());
                    e.version += 1;
                    e.clone()
                }
                None => {
                    let e = Entry {
                        value: Zeroizing::new(value.expose().to_string()),
                        version: 1,
                        created_at: OffsetDateTime::now_utc(),
                        rotated_at: None,
                    };
                    map.insert(rref.path(), e.clone());
                    e
                }
            };
            Ok((entry.record(), true))
        })
    }

    fn rm(&self, env: &Env, rref: &Ref) -> Result<bool, VaultError> {
        self.mutate(|p| {
            let map = match p.envs.get_mut(env.as_str()) {
                Some(m) => m,
                None => return Ok((false, false)),
            };
            let removed = map.remove(&rref.path()).is_some();
            Ok((removed, removed))
        })
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
        self.mutate(|p| {
            let map = p.envs.entry(env.as_str().to_string()).or_default();
            let e = map
                .get_mut(&rref.path())
                .ok_or_else(|| VaultError::NotFound {
                    rref: rref.path(),
                    env: env.as_str().to_string(),
                })?;
            e.value = Zeroizing::new(value.expose().to_string());
            e.version += 1;
            e.rotated_at = Some(OffsetDateTime::now_utc());
            let entry = e.clone();
            Ok((entry.record(), true))
        })
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

/// `chmod 0600` — the vault file
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
    fn ciphertext_hides_values() {
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
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
    fn set_private_on_missing_file_is_an_error() {
        assert!(set_private(Path::new("/nonexistent/x/y")).is_err());
    }

    #[test]
    fn default_vault_path_follows_xdg_then_home() {
        temp_env::with_vars(
            [
                ("XDG_DATA_HOME", Some("/tmp/xdg-d")),
                ("HOME", Some("/tmp/fakehome")),
            ],
            || {
                assert_eq!(
                    AgeVault::default_vault_path(),
                    PathBuf::from("/tmp/xdg-d/arca/vault.age")
                );
            },
        );
        // no XDG → HOME-based fallback
        temp_env::with_vars(
            [
                ("XDG_DATA_HOME", None::<&str>),
                ("HOME", Some("/tmp/fakehome")),
            ],
            || {
                assert_eq!(
                    AgeVault::default_vault_path(),
                    PathBuf::from("/tmp/fakehome/.local/share/arca/vault.age")
                );
            },
        );
        // no HOME either → "."
        temp_env::with_vars(
            [("XDG_DATA_HOME", None::<&str>), ("HOME", None::<&str>)],
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
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        // a valid extra recipient opens and reports the identity key
        let extra = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, id.clone(), &[extra.to_public().to_string()]).unwrap();
        assert_eq!(v.public_recipient(), id.to_public().to_string());
        // an invalid recipient string is a config error
        assert!(AgeVault::open(&v_path, id.clone(), &["not-a-recipient".into()]).is_err());
        // a directory where the vault file should be → i/o error
        let as_dir = dir.path().join("vaultdir");
        std::fs::create_dir(&as_dir).unwrap();
        assert!(AgeVault::open(&as_dir, id.clone(), &[]).is_err());
        // garbage where ciphertext should be → crypto error
        let garbage = dir.path().join("garbage.age");
        std::fs::write(&garbage, b"definitely not age").unwrap();
        assert!(AgeVault::open(&garbage, id, &[]).is_err());
    }

    #[test]
    fn open_or_init_bootstraps_then_opens() {
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let src = arca_identity_file::FileIdentity::new(dir.path().join("id.txt"));
        let (_v, created) = AgeVault::open_or_init(&v_path, &src, &[]).unwrap();
        assert!(created);
        let (_v, created) = AgeVault::open_or_init(&v_path, &src, &[]).unwrap();
        assert!(!created);
        // identity can't be stored → error
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let bad_src = arca_identity_file::FileIdentity::new(blocker.join("id.txt"));
        assert!(AgeVault::open_or_init(&v_path, &bad_src, &[]).is_err());
        // identity exists but is garbage → open fails
        let bad = dir.path().join("bad.txt");
        std::fs::write(&bad, "junk\n").unwrap();
        let bad_src = arca_identity_file::FileIdentity::new(&bad);
        assert!(AgeVault::open_or_init(&v_path, &bad_src, &[]).is_err());
    }

    #[test]
    fn decrypt_reports_every_failure_mode() {
        let id = x25519::Identity::generate();
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
        let e = AgeVault::decrypt(&id, &blob).err().unwrap();
        assert!(e.to_string().contains("passphrase"), "{e}");
        // encrypted to someone else → can't decrypt
        let other = x25519::Identity::generate();
        assert!(AgeVault::decrypt(&id, &encrypt_to(&other.to_public(), b"{}")).is_err());
        // valid envelope, garbage plaintext → payload error
        let blob = encrypt_to(&id.to_public(), b"not json");
        assert!(AgeVault::decrypt(&id, &blob).is_err());
        // a newer format version is refused, honestly
        let blob = encrypt_to(&id.to_public(), br#"{"v":99,"envs":{}}"#);
        let e = AgeVault::decrypt(&id, &blob).err().unwrap();
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
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let plain = br#"{"v":1,"envs":{"dev":{"x/y":{"value":"fake-old","created_at":"2024-01-01T00:00:00Z"}}}}"#;
        std::fs::write(&v_path, encrypt_to(&id.to_public(), plain)).unwrap();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
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
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
        // die holding the lock → the mutex is poisoned for good
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = v.state.lock().unwrap();
            panic!("intentional poison");
        }));
        let r = Ref::new("arca://x/y").unwrap();
        let e = Env::dev();
        // every verb reports the poisoned state as a backend error
        for e in [
            v.get(&e, &r).err().unwrap(),
            v.put(&e, &r, Secret::new("f")).err().unwrap(),
            v.rm(&e, &r).err().unwrap(),
            v.list(&e).err().unwrap(),
            v.rotate(&e, &r, Secret::new("f")).err().unwrap(),
        ] {
            assert!(format!("{e:?}").starts_with("Backend"), "{e:?}");
        }
    }

    #[test]
    fn save_surfaces_filesystem_failures() {
        let dir = tempfile::tempdir().unwrap();
        let id = x25519::Identity::generate();
        let r = Ref::new("arca://x/y").unwrap();

        // parent dir swapped for a file → create_dir_all can't proceed
        let parent = dir.path().join("swapped");
        let v_path = parent.join("vault.age");
        let v = AgeVault::open(&v_path, id.clone(), &[]).unwrap();
        std::fs::write(&parent, "not a dir").unwrap();
        assert!(v.put(&Env::dev(), &r, Secret::new("fake")).is_err());
        let _ = std::fs::remove_file(&parent);

        // vault path became a directory → the rename can't land
        let v_path = dir.path().join("isdir.age");
        let v = AgeVault::open(&v_path, id.clone(), &[]).unwrap();
        std::fs::create_dir(&v_path).unwrap();
        assert!(v.put(&Env::dev(), &r, Secret::new("fake")).is_err());

        // read-only vault dir → every mutating verb's save fails
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = dir.path().join("ro");
            std::fs::create_dir(&sub).unwrap();
            let v_path = sub.join("vault.age");
            let v = AgeVault::open(&v_path, id, &[]).unwrap();
            v.put(&Env::dev(), &r, Secret::new("fake")).unwrap();
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
            assert!(v.put(&Env::dev(), &r, Secret::new("fake2")).is_err());
            assert!(v.rm(&Env::dev(), &r).is_err());
            assert!(v.rotate(&Env::dev(), &r, Secret::new("fake3")).is_err());
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn a_parentless_vault_path_skips_dir_setup() {
        // "" has no parent — save() skips create_dir_all and the
        // tmp write itself fails. An error, not a panic.
        let id = x25519::Identity::generate();
        let v = AgeVault::open(Path::new(""), id, &[]).unwrap();
        let r = Ref::new("arca://x/y").unwrap();
        assert!(v.put(&Env::dev(), &r, Secret::new("fake")).is_err());
    }

    #[test]
    fn writes_serialize_across_vault_handles() {
        // Two handles on one file — the multi-process shape. The
        // second handle's put must merge the first's write (the
        // locked re-read), not clobber it (MED-3).
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v1 = AgeVault::open(&v_path, id.clone(), &[]).unwrap();
        let v2 = AgeVault::open(&v_path, id, &[]).unwrap();
        let r1 = Ref::new("arca://a/one").unwrap();
        let r2 = Ref::new("arca://b/two").unwrap();
        v1.put(&Env::dev(), &r1, Secret::new("fake-1")).unwrap();
        v2.put(&Env::dev(), &r2, Secret::new("fake-2")).unwrap();
        // both writes landed — the writer's own view is merged
        let refs = v2.list(&Env::dev()).unwrap();
        assert!(refs.contains(&r1) && refs.contains(&r2), "{refs:?}");
        // reads are open-time cached — the other handle doesn't see
        // the late write until its next mutate re-reads the file
        assert!(v1.get(&Env::dev(), &r2).unwrap().is_none());
        v1.put(
            &Env::dev(),
            &Ref::new("arca://c/three").unwrap(),
            Secret::new("f3"),
        )
        .unwrap();
        assert_eq!(
            v1.get(&Env::dev(), &r2).unwrap().unwrap().value.expose(),
            "fake-2"
        );
        // the lockfile is gone once the verb returns
        assert!(!v_path.with_extension("age.lock").exists());
    }

    #[test]
    fn a_held_lock_fails_the_write_and_a_stale_one_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
        let r = Ref::new("arca://x/y").unwrap();
        let lock = v_path.with_extension("age.lock");

        // a fresh lockfile → someone else holds it; the verb waits
        // LOCK_WAIT then reports the lock, never panics
        std::fs::write(&lock, format!("0 {}", unix_ms())).unwrap();
        let e = v.put(&Env::dev(), &r, Secret::new("fake")).unwrap_err();
        assert!(e.to_string().contains("locked by another process"), "{e}");

        // a stale lockfile → the holder is presumed dead; the lock
        // is reclaimed and the write lands
        std::fs::write(&lock, "0 1").unwrap();
        v.put(&Env::dev(), &r, Secret::new("fake")).unwrap();
        assert!(!lock.exists());
    }

    #[test]
    fn a_mutate_reread_surfaces_a_broken_on_disk_vault() {
        // the file decrypts at open, then is swapped for garbage —
        // the locked re-read reports it instead of clobbering
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
        std::fs::write(&v_path, b"not age anymore").unwrap();
        let e = v
            .put(
                &Env::dev(),
                &Ref::new("arca://x/y").unwrap(),
                Secret::new("f"),
            )
            .unwrap_err();
        assert!(format!("{e:?}").starts_with("Crypto"), "{e}");
    }

    #[test]
    fn list_rejects_a_stored_key_that_isnt_a_ref() {
        // put validates refs, so only a hand-edited payload could
        // carry a bad key — list must error, never panic
        let dir = tempfile::tempdir().unwrap();
        let v_path = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let plain = br#"{"v":1,"envs":{"dev":{"bad key!":{"value":"fake","version":1,"created_at":"2024-01-01T00:00:00Z"}}}}"#;
        std::fs::write(&v_path, encrypt_to(&id.to_public(), plain)).unwrap();
        let v = AgeVault::open(&v_path, id, &[]).unwrap();
        assert!(v.list(&Env::dev()).is_err());
    }
}
