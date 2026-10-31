// arca-vault-cf worker — the `arca-vault-cf/1` wire protocol over
// Cloudflare Workers + D1. See ../src/lib.rs for the client.
//
// THE WORKER NEVER SEES PLAINTEXT. Values arrive as base64 age
// ciphertext produced client-side by arca-vault-cf and are stored
// verbatim in D1 rows; this file contains no decryption capability
// and no key material. Bookkeeping (version bump, created_at
// preservation, rotated_at stamping) is server-side so concurrent
// writers can't race client-computed versions.
//
// Routes (all under /v1, bearer-authed):
//   GET    /v1/{env}           → {"refs": ["a/b", …]}
//   GET    /v1/{env}/{ref…}    → {"ciphertext","version","created_at","rotated_at"}
//   PUT    /v1/{env}/{ref…}    {"ciphertext","rotate"} → record metadata
//   DELETE /v1/{env}/{ref…}    → {"removed": bool}
// Errors: {"error": "…"} with a non-2xx status.

const JSON = { "content-type": "application/json" };

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: JSON });
}

function err(status, msg) {
  return json({ error: msg }, status);
}

export default {
  async fetch(request, env) {
    // Bearer check first — the token lives in a Worker secret
    // (`wrangler secret put ARCA_VAULT_TOKEN`), never in the repo.
    const want = `Bearer ${env.ARCA_VAULT_TOKEN}`;
    if (request.headers.get("authorization") !== want) {
      return err(401, "unauthorized");
    }

    const url = new URL(request.url);
    const rest = url.pathname.startsWith("/v1/")
      ? url.pathname.slice("/v1/".length)
      : null;
    if (rest === null) return err(404, "no such route");

    // /v1/{env} lists; /v1/{env}/{ref…} addresses one record. Ref
    // paths contain '/' by design.
    const slash = rest.indexOf("/");
    const envName = slash === -1 ? rest : rest.slice(0, slash);
    const refPath = slash === -1 ? null : rest.slice(slash + 1);
    if (!envName || refPath === "") return err(404, "no such route");

    const db = env.DB;

    try {
      if (refPath === null) {
        if (request.method !== "GET") return err(405, "method not allowed");
        const { results } = await db
          .prepare("SELECT ref FROM secrets WHERE env = ? ORDER BY ref")
          .bind(envName)
          .all();
        return json({ refs: results.map((r) => r.ref) });
      }

      switch (request.method) {
        case "GET": {
          const row = await db
            .prepare(
              "SELECT ciphertext, version, created_at, rotated_at FROM secrets WHERE env = ? AND ref = ?"
            )
            .bind(envName, refPath)
            .first();
          if (!row) return err(404, "not found");
          return json(row);
        }

        case "PUT": {
          let body;
          try {
            body = await request.json();
          } catch {
            return err(400, "bad json body");
          }
          if (typeof body.ciphertext !== "string" || !body.ciphertext) {
            return err(400, "missing ciphertext");
          }
          const rotate = body.rotate === true;
          const now = new Date().toISOString();

          const old = await db
            .prepare(
              "SELECT version, created_at, rotated_at FROM secrets WHERE env = ? AND ref = ?"
            )
            .bind(envName, refPath)
            .first();

          if (!old && rotate) return err(404, "not found");

          const version = old ? old.version + 1 : 1;
          const created_at = old ? old.created_at : now;
          const rotated_at = rotate ? now : old ? old.rotated_at : null;

          await db
            .prepare(
              `INSERT INTO secrets (env, ref, ciphertext, version, created_at, rotated_at)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT (env, ref) DO UPDATE SET
                 ciphertext = excluded.ciphertext,
                 version = excluded.version,
                 rotated_at = excluded.rotated_at`
            )
            .bind(envName, refPath, body.ciphertext, version, created_at, rotated_at)
            .run();

          return json({ version, created_at, rotated_at });
        }

        case "DELETE": {
          const { meta } = await db
            .prepare("DELETE FROM secrets WHERE env = ? AND ref = ?")
            .bind(envName, refPath)
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
