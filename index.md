---
layout: home

hero:
  name: arca
  text: secrets broker for agents — sealed, not spilled
  tagline: arca:// references in code, env vars at runtime, an audit trail for every materialization
  actions:
    - theme: brand
      text: Getting Started
      link: /getting-started
    - theme: alt
      text: Tutorial
      link: /tutorials/first-secret

features:
  - icon: 🦪
    title: References, not values
    details: arca://name URIs in code and config — nothing sensitive lives in the repo at all
    link: /explanation/architecture
    linkText: architecture
  - icon: 🤖
    title: Agent-first
    details: arca run injects secrets as env vars — agents run commands without ever seeing keys; --json + manifest/schema everywhere
    link: /reference/cli
    linkText: CLI reference
  - icon: 🗄️
    title: Pluggable vaults
    details: the arca-vault port — local age-encrypted file first, a self-hosted Cloudflare Worker + D1 backend too; ciphertext-only everywhere
    link: /how-to/route-envs-to-vaults
    linkText: env routing
  - icon: 🔏
    title: Audited materialization
    details: every resolve, denial, and failure records actor, ref, env, timestamp — an ndjson wire on stderr; Secret redacts by construction
    link: /reference/audit-events
    linkText: audit contract
---
