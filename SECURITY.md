# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/arca/security/advisories/new

Do not open public issues for security reports.

## Scope

- `arca` writes only inside the repo's `project/` worktree — every
  mutation is one atomic, attributable commit on the `project`
  branch; history is the audit trail.
- Config (`arca.toml`) holds names and paths, never secrets; secrets
  resolve via env var → OS keyring → `~/.config/arca/env` (chmod 600)
  when a phase needs them.
- Item IDs and revisions are path-validated before they touch file
  paths or git args — no `..`, no `/`, bounded length.

## Notes

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
