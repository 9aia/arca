//! `OpsError` — one error type for the front-ends to match.

use arca_core::{ParseActorError, ParseEnvError, ParseRefError};
use arca_deploy::DeployError;
use arca_vault::VaultError;

/// Everything ops can fail with — config, resolution, vault.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// Config file load/parse/vault-kind failure.
    #[error("config: {0}")]
    Config(String),
    /// Bad env name (`--env`/`ARCA_ENV`).
    #[error(transparent)]
    Env(#[from] ParseEnvError),
    /// Bad `arca://` ref.
    #[error(transparent)]
    Ref(#[from] ParseRefError),
    /// Bad actor attribution (`--actor`/`ARCA_ACTOR`).
    #[error(transparent)]
    Actor(#[from] ParseActorError),
    /// Vault backend failure.
    #[error(transparent)]
    Vault(#[from] VaultError),
    /// Deploy target failure.
    #[error(transparent)]
    Deploy(#[from] DeployError),
    /// Identity backend failure (keyfile, session keyring).
    #[error(transparent)]
    Identity(#[from] arca_identity::IdentityError),
    /// A declared/ad-hoc secret isn't in the vault.
    #[error("{var} → {rref} is not stored in env {env}")]
    NotStored {
        /// The env-var name that wanted it.
        var: String,
        /// The ref it pointed at.
        rref: String,
        /// The env that was queried.
        env: String,
    },
    /// stdin/stdout/process I/O failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Catch-all with a ready-made message.
    #[error("{0}")]
    Msg(String),
}
