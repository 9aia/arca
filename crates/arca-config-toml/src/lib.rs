//! TOML impl of `ConfigSource` — explicit path → $ARCA_CONFIG →
//! ./arca.toml → ~/.config/arca/arca.toml.

use arca_config::ConfigSource;
use std::path::PathBuf;

/// File-backed config source.
pub struct TomlSource {
    /// Explicit `--config` path, or None for the default search.
    pub path: Option<String>,
}

impl TomlSource {
    /// The candidate files, in search order.
    pub fn candidates(&self) -> Vec<PathBuf> {
        match self
            .path
            .clone()
            .or_else(|| std::env::var("ARCA_CONFIG").ok())
        {
            Some(p) => vec![PathBuf::from(p)],
            None => vec![
                PathBuf::from("arca.toml"),
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or(".".into())
                    .join(".config/arca/arca.toml"),
            ],
        }
    }

    /// The first candidate that exists, if any.
    pub fn locate(&self) -> Option<PathBuf> {
        self.candidates().into_iter().find(|p| p.exists())
    }
}

impl ConfigSource for TomlSource {
    fn load(&self) -> Result<arca_core::Config, String> {
        match self.locate() {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()))
            }
            None => Err("no arca.toml found (search: $ARCA_CONFIG, ., ~/.config/arca/)".into()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn explicit_missing_file_errors() {
        let src = TomlSource {
            path: Some("/nonexistent/arca.toml".into()),
        };
        let err = src.load().err().unwrap_or_default();
        assert!(err.contains("arca.toml"));
    }

    #[test]
    fn parses_secrets_table() {
        let dir = std::env::temp_dir().join(format!("arca-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let f = dir.join("arca.toml");
        std::fs::write(&f, "[secrets]\nSTRIPE_KEY = \"arca://billing/stripe\"\n")
            .unwrap_or_default();
        let src = TomlSource {
            path: Some(f.to_string_lossy().into_owned()),
        };
        let cfg = src.load().unwrap_or_default();
        assert_eq!(cfg.secrets.len(), 1);
        assert!(cfg.is_known_env("prod"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_fail_loud() {
        let dir = std::env::temp_dir().join(format!("arca-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let f = dir.join("arca.toml");
        std::fs::write(&f, "project_dir = \"project\"\n").unwrap_or_default();
        let src = TomlSource {
            path: Some(f.to_string_lossy().into_owned()),
        };
        assert!(src.load().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_search_builds_candidates() {
        // no explicit path, no ARCA_CONFIG → ./arca.toml +
        // ~/.config/arca/arca.toml are probed (and miss in a
        // bare cwd)
        let src = TomlSource { path: None };
        temp_env::with_vars(
            [
                ("ARCA_CONFIG", None::<&str>),
                ("HOME", Some("/nonexistent-home")),
            ],
            || {
                let dir =
                    std::env::temp_dir().join(format!("arca-cfg-none-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let cwd = std::env::current_dir().unwrap();
                std::env::set_current_dir(&dir).unwrap();
                // no arca.toml anywhere reachable → search error
                let first = src.load();
                assert!(first.is_err(), "expected search error: {first:?}");
                assert!(src.locate().is_none());
                // ./arca.toml is found on the default search
                std::fs::write(dir.join("arca.toml"), "[envs]\nci = \"local\"\n").unwrap();
                let out = src.load().unwrap();
                assert_eq!(
                    src.locate().as_deref().and_then(|p| p.file_name()),
                    Some(std::ffi::OsStr::new("arca.toml"))
                );
                std::env::set_current_dir(cwd).unwrap();
                assert!(out.envs.contains_key(&"ci".parse().unwrap()));
                let _ = std::fs::remove_dir_all(&dir);
            },
        );
    }
}
