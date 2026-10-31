# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/arca/security/advisories/new

Do not open public issues for security reports.

## Scope

- arca materializes secrets — `arca://` refs in code/config,
  age-encrypted vaults, env-var injection, an ndjson audit wire on
  stderr. See `docs/explanation/threat-model.md` (docs branch) for
  what it does and does not defend against.
- `arca.toml` holds names, paths, and routing — never values;
  credentials reach vaults via env vars (`token_env` names, not
  inline).
- `identity.txt` (0600) is the master key for every vault impl —
  treat it like a private SSH key.
- Refs and env/actor names are validated before they touch store
  keys — strict segment rules, bounded length, no traversal.

## Notes

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
