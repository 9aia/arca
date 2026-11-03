// arca-vault-cf worker — the `arca-vault-cf/2` wire protocol over
// Cloudflare Workers + D1. See ../src/lib.rs for the client.
//
// THE WORKER NEVER SEES PLAINTEXT. Values arrive as base64 age
// ciphertext produced client-side by arca-vault-cf and are stored
// verbatim in D1 rows; this file contains no decryption capability
// and no key material. Bookkeeping (version bump, created_at
// preservation, rotated_at stamping) is server-side so concurrent
// writers can't race client-computed versions. When the client runs
// with `private_refs = true` (P-004) even the `{key}` path segment
// is an opaque HMAC-derived hex string and the row's `name` column
// is an age-encrypted blob — the store can't tell which secrets
// exist, only that some do.
//
// Routes (all under /v1, authed + scoped):
//   GET    /v1/{env}           → {"refs": [{"key","name"}, …]}
//   GET    /v1/{env}/{key…}    → {"ciphertext","name","version","created_at","rotated_at"}
//   PUT    /v1/{env}/{key…}    {"ciphertext","name"?,"rotate"} → record metadata
//   DELETE /v1/{env}/{key…}    → {"removed": bool}
// Errors: {"error": "…"} with a non-2xx status.
//
// Auth — wire `arca-vault-cf/2` (P-003):
//   Authorization: Bearer <token>
//     Legacy mode, still accepted during migration.
//   Authorization: ArcaHmac <token_id>, <ts>, <sig>
//     sig = hex HMAC-SHA256(token, method+"\n"+path+"\n"+ts+"\n"+sha256hex(body)).
//     The token never travels; |now - ts| must be ≤ 60s (replay window).
//
// Token config — `wrangler secret put ARCA_VAULT_TOKENS` (JSON):
//   {"<token_id>": {"token": "<secret>",
//                   "scopes": {"<env>"|"*": {"read": true, "write": true}}}}
// `ARCA_VAULT_TOKEN` (single string secret) is the legacy fallback:
// one bearer token with read+write on every env. Rotation is a
// `wrangler secret put` away — no admin route.

const JSON = { "content-type": "application/json" };

const REPLAY_WINDOW_S = 60;

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: JSON });
}

function err(status, msg) {
  return json({ error: msg }, status);
}

function hex(bytes) {
  return [...new Uint8Array(bytes)]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

// Constant-time string equality (P-006): both sides are byte-decoded
// and XOR-folded over the *longer* length — no early exit on the
// first mismatching byte, and a length difference is folded into the
// accumulator instead of short-circuiting. Missing bytes read as 0.
function ctEq(a, b) {
  const enc = new TextEncoder();
  const x = enc.encode(a);
  const y = enc.encode(b);
  let diff = x.length ^ y.length;
  const n = Math.max(x.length, y.length);
  for (let i = 0; i < n; i++) {
    diff |= (x[i] ?? 0) ^ (y[i] ?? 0);
  }
  return diff === 0;
}

async function sha256Hex(text) {
  return hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
}

async function hmacSha256Hex(secret, message) {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  return hex(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(message)));
}

// The scoped-token table from `ARCA_VAULT_TOKENS` JSON. A malformed
// secret yields an empty table — requests then fail closed (401),
// never silently fall through to legacy auth.
function tokenTable(env) {
  const table = {};
  if (typeof env.ARCA_VAULT_TOKENS === "string" && env.ARCA_VAULT_TOKENS) {
    try {
      Object.assign(table, JSON.parse(env.ARCA_VAULT_TOKENS));
    } catch {
      // invalid JSON → empty table → every token lookup misses
    }
  }
  return table;
}

const FULL_SCOPE = { read: true, write: true };

// Verify the Authorization header against the token table plus the
// legacy `ARCA_VAULT_TOKEN`. Returns the caller's scope map or null.
async function authenticate(request, env, path, bodyText) {
  const header = request.headers.get("authorization") || "";
  const table = tokenTable(env);

  if (header.startsWith("Bearer ")) {
    const candidate = header.slice("Bearer ".length);
    let scopes = null;
    // Compare against every configured token — no early exit on a
    // non-match, so the loop count doesn't reveal which entry was
    // hit. A shared value resolves to the last matching entry.
    for (const id of Object.keys(table)) {
      const entry = table[id];
      if (entry && typeof entry.token === "string" && ctEq(candidate, entry.token)) {
        scopes = entry.scopes || null;
      }
    }
    if (typeof env.ARCA_VAULT_TOKEN === "string" && env.ARCA_VAULT_TOKEN) {
      if (ctEq(candidate, env.ARCA_VAULT_TOKEN)) {
        scopes = { "*": FULL_SCOPE };
      }
    }
    return scopes;
  }

  if (header.startsWith("ArcaHmac ")) {
    const parts = header
      .slice("ArcaHmac ".length)
      .split(",")
      .map((s) => s.trim());
    if (parts.length !== 3) return null;
    const [id, ts, sig] = parts;
    const entry = table[id];
    if (!entry || typeof entry.token !== "string") return null;
    const t = Number(ts);
    const now = Math.floor(Date.now() / 1000);
    if (!Number.isFinite(t) || Math.abs(now - t) > REPLAY_WINDOW_S) return null;
    const expect = await hmacSha256Hex(
      entry.token,
      `${request.method}\n${path}\n${ts}\n${await sha256Hex(bodyText)}`
    );
    return ctEq(sig, expect) ? entry.scopes || null : null;
  }

  return null;
}

// `scopes` is `{env|"*": {read, write}}` — env entries win over the
// wildcard; a missing capability denies.
function scopeAllows(scopes, envName, write) {
  const s = (scopes && (scopes[envName] || scopes["*"])) || {};
  return write ? s.write === true : s.read === true;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const rest = url.pathname.startsWith("/v1/")
      ? url.pathname.slice("/v1/".length)
      : null;
    if (rest === null) return err(404, "no such route");

    // /v1/{env} lists; /v1/{env}/{key…} addresses one record. Key
    // paths contain '/' by design (a ref path, or one opaque
    // HMAC-derived hex segment under private_refs).
    const slash = rest.indexOf("/");
    const envName = slash === -1 ? rest : rest.slice(0, slash);
    const keyPath = slash === -1 ? null : rest.slice(slash + 1);
    if (!envName || keyPath === "") return err(404, "no such route");

    const isRead = request.method === "GET";
    const isWrite = request.method === "PUT" || request.method === "DELETE";
    if (!isRead && !isWrite) return err(405, "method not allowed");

    // Auth after routing so the scope check knows env+verb; the
    // body text is read once — it feeds both signature
    // verification and the PUT handler.
    const bodyText = await request.text();
    const scopes = await authenticate(request, env, url.pathname, bodyText);
    if (scopes === null) return err(401, "unauthorized");
    if (!scopeAllows(scopes, envName, isWrite)) {
      return err(403, "token scope denies this env/verb");
    }

    const db = env.DB;

    try {
      if (keyPath === null) {
        const { results } = await db
          .prepare("SELECT ref, name FROM secrets WHERE env = ? ORDER BY ref")
          .bind(envName)
          .all();
        return json({ refs: results.map((r) => ({ key: r.ref, name: r.name })) });
      }

      switch (request.method) {
        case "GET": {
          const row = await db
            .prepare(
              "SELECT ciphertext, name, version, created_at, rotated_at FROM secrets WHERE env = ? AND ref = ?"
            )
            .bind(envName, keyPath)
            .first();
          if (!row) return err(404, "not found");
          return json(row);
        }

        case "PUT": {
          let body;
          try {
            body = JSON.parse(bodyText);
          } catch {
            return err(400, "bad json body");
          }
          if (typeof body.ciphertext !== "string" || !body.ciphertext) {
            return err(400, "missing ciphertext");
          }
          // `name` is the private-refs name blob (age ciphertext,
          // opaque here); null on plain rows.
          const name = typeof body.name === "string" ? body.name : null;
          const rotate = body.rotate === true;
          const now = new Date().toISOString();

          const old = await db
            .prepare(
              "SELECT version, created_at, rotated_at FROM secrets WHERE env = ? AND ref = ?"
            )
            .bind(envName, keyPath)
            .first();

          if (!old && rotate) return err(404, "not found");

          const version = old ? old.version + 1 : 1;
          const created_at = old ? old.created_at : now;
          const rotated_at = rotate ? now : old ? old.rotated_at : null;

          await db
            .prepare(
              `INSERT INTO secrets (env, ref, ciphertext, name, version, created_at, rotated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT (env, ref) DO UPDATE SET
                 ciphertext = excluded.ciphertext,
                 name = excluded.name,
                 version = excluded.version,
                 rotated_at = excluded.rotated_at`
            )
            .bind(envName, keyPath, body.ciphertext, name, version, created_at, rotated_at)
            .run();

          return json({ version, created_at, rotated_at });
        }

        case "DELETE": {
          const { meta } = await db
            .prepare("DELETE FROM secrets WHERE env = ? AND ref = ?")
            .bind(envName, keyPath)
            .run();
          return json({ removed: (meta?.changes ?? 0) > 0 });
        }

        default:
          return err(405, "method not allowed");
      }
    } catch (e) {
      // A D1/infra failure — never leak internals beyond a message.
      return err(500, `backend: ${e.message}`);
    }
  },
};
