#!/bin/sh
# pdp-campaign.sh — example campaign-local policy decision point for
# `cuora serve`'s `[serve.pdp] exec` wire (ADR-0003). Demonstrates the
# real consult contract the campaign wants peregrin to own one day:
# the request lands on stdin as one JSON line
# ({actor, attestation?, op, env, ref?}); the verdict leaves on stdout
# ({"allow": bool, "reason"?, "obligations"?}).
#
# Policy here is deliberately small — the campaign's own shape:
#   · prod materialization is never delegated to a campaign actor
#   · destructive ops (rm/rotate) need a stronger decider than us
#   · everything else in dev/staging is allowed
# Peregrin replacing this file is tracked in cuora's project ledger —
# the exec line stays identical, the command becomes `peregrin …`.
#
# Consults are append-logged when CUORA_PDP_LOG is set (debugging).

req=$(cat)
[ -n "${CUORA_PDP_LOG:-}" ] && printf '%s\n' "$req" >> "$CUORA_PDP_LOG"

case "$req" in
    *'"env":"prod"'*)
        printf '%s' '{"allow":false,"reason":"prod materialization is not delegated to campaign actors"}'
        ;;
    *'"op":"rm"'*|*'"op":"rotate"'*)
        printf '%s' '{"allow":false,"reason":"rm/rotate require an interactive policy decision"}'
        ;;
    *)
        printf '%s' '{"allow":true,"reason":"campaign policy: non-prod read/write op"}'
        ;;
esac
