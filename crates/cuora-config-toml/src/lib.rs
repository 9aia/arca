//! TOML impl of `ConfigSource` — explicit path → $CUORA_CONFIG →
//! $ARCA_CONFIG (legacy) → ./cuora.toml → ./arca.toml (legacy) →
//! ~/.config/cuora/cuora.toml → ~/.config/arca/arca.toml (legacy).

use cuora_config::ConfigSource;
use std::path::PathBuf;

/// File-backed config source.
pub struct TomlSource {
    /// Explicit `--config` path, or None for the default search.
    pub path: Option<String>,
}

impl TomlSource {
    /// The candidate files, in search order.
    pub fn candidates(&self) -> Vec<PathBuf> {
        let read = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
        let env_path = read("CUORA_CONFIG").or_else(|| read("ARCA_CONFIG"));
        match self.path.clone().or(env_path) {
            Some(p) => vec![PathBuf::from(p)],
            None => {
                let home = std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or(".".into());
                vec![
                    PathBuf::from("cuora.toml"),
                    PathBuf::from("arca.toml"),
                    home.join(".config/cuora/cuora.toml"),
                    home.join(".config/arca/arca.toml"),
                ]
            }
        }
    }

    /// The first candidate that exists, if any.
    pub fn locate(&self) -> Option<PathBuf> {
        self.candidates().into_iter().find(|p| p.exists())
    }
}

impl ConfigSource for TomlSource {
    fn load(&self) -> Result<cuora_core::Config, String> {
        match self.locate() {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()))
            }
            None => Err(
                "no cuora.toml found (search: $CUORA_CONFIG/$ARCA_CONFIG, ./cuora.toml, ./arca.toml, ~/.config/cuora/, ~/.config/arca/)"
                    .into(),
            ),
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
            path: Some("/nonexistent/cuora.toml".into()),
        };
        let err = src.load().err().unwrap_or_default();
        assert!(err.contains("cuora.toml"));
    }

    #[test]
    fn located_but_unreadable_errors() {
        // A directory exists (so `locate` picks it) but can't be
        // read as a file — the read error surfaces, not the
        // "not found" search error.
        let dir = std::env::temp_dir().join(format!("cuora-cfg-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = TomlSource {
            path: Some(dir.to_string_lossy().into_owned()),
        };
        assert_eq!(src.locate().as_deref(), Some(dir.as_path()));
        let err = src.load().unwrap_err();
        assert!(err.contains("read"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_secrets_table() {
        let dir = std::env::temp_dir().join(format!("cuora-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let f = dir.join("cuora.toml");
        std::fs::write(&f, "[secrets]\nSTRIPE_KEY = \"cuora://billing/stripe\"\n")
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
        let dir = std::env::temp_dir().join(format!("cuora-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let f = dir.join("cuora.toml");
        std::fs::write(&f, "project_dir = \"project\"\n").unwrap_or_default();
        let src = TomlSource {
            path: Some(f.to_string_lossy().into_owned()),
        };
        assert!(src.load().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_search_builds_candidates() {
        // no explicit path, no CUORA_CONFIG/ARCA_CONFIG → ./cuora.toml +
        // ./arca.toml + ~/.config/{cuora,arca}/… are probed (and miss in a
        // bare cwd)
        let src = TomlSource { path: None };
        temp_env::with_vars(
            [
                ("CUORA_CONFIG", None::<&str>),
                ("ARCA_CONFIG", None::<&str>),
                ("HOME", Some("/nonexistent-home")),
            ],
            || {
                let dir =
                    std::env::temp_dir().join(format!("cuora-cfg-none-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let cwd = std::env::current_dir().unwrap();
                std::env::set_current_dir(&dir).unwrap();
                // no cuora.toml anywhere reachable → search error
                let first = src.load();
                assert!(first.is_err(), "expected search error: {first:?}");
                assert!(src.locate().is_none());
                // ./cuora.toml is found on the default search
                std::fs::write(dir.join("cuora.toml"), "[envs]\nci = \"local\"\n").unwrap();
                let out = src.load().unwrap();
                assert_eq!(
                    src.locate().as_deref().and_then(|p| p.file_name()),
                    Some(std::ffi::OsStr::new("cuora.toml"))
                );
                std::env::set_current_dir(&cwd).unwrap();
                assert!(out.envs.contains_key(&"ci".parse().unwrap()));
                // legacy ./arca.toml is still found when cuora.toml
                // is absent
                std::fs::remove_file(dir.join("cuora.toml")).unwrap();
                std::fs::write(dir.join("arca.toml"), "[envs]\nci = \"local\"\n").unwrap();
                std::env::set_current_dir(&dir).unwrap();
                let out = src.load().unwrap();
                assert!(out.envs.contains_key(&"ci".parse().unwrap()));
                std::env::set_current_dir(cwd).unwrap();
                let _ = std::fs::remove_dir_all(&dir);
            },
        );
    }
}
