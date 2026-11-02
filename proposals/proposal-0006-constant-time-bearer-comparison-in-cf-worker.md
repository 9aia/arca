+++
id = "P-006"
title = "constant-time bearer comparison in cf worker"
status = "in-progress"
created = "2026-11-02"
priority = "low"
labels = ["security", "vault-cf"]
+++

Status: **proposal**. Trivial hardening in `worker/index.js`.

## Problem

The worker's auth check compares the Authorization header with
`!==` — early-exit string compare leaks prefix length via timing.
Largely theoretical over TLS with a high-entropy token, but the
fix is a few lines and removes the question entirely.

## Proposal

Constant-time equality in the worker: decode both values to
bytes, XOR-fold over the longer length (no early exit), or use
`crypto.subtle.timingSafeEqual` if available in the Workers
runtime. Reject length mismatch without short-circuiting the
compare loop.

## Effort

Trivial — ~5 lines + a test asserting equal-time behavior is
not measurable as prefix-dependent. Bundle with P-003's worker
touch so the wire file is edited once.
