# Templates (E4-4)

Copyable starting points for the three things new users build first: a
**flow** (DSL JSON), an **external-task worker** (Rust crate), and an
**MCP client wiring** (config JSON).

| Template | What it is | Start at |
|---|---|---|
| [`flows/`](flows/README.md) | Machine-readable DSL v1 flow documents (basic gateway, AI approval, timer wait), compiled by a workspace test | `flows/README.md` |
| [`ai-worker/`](ai-worker/README.md) | A standalone, compiling worker crate: Worker SDK poll loop + E3-3 tool security boundary + E3-1 AI task envelope | `ai-worker/README.md` |
| [`mcp-config.json`](mcp-config.json) | MCP client configuration for the E4-2 server (`bpm-engine-mcp-server`) | field reference below |

Companion guides: [15-minute quickstart](../docs/quickstart.md),
[troubleshooting](../docs/troubleshooting.md),
[runnable examples index](../docs/examples/index.md).

## `mcp-config.json` field reference

JSON has no comments, so the fields are documented here. Drop this file
into any stdio MCP client that reads an `mcpServers` block (Claude
Desktop, Claude Code, Cursor, `mcphost`, …) — see `docs/mcp-server.md`
for the full protocol and security story.

```json
{
  "mcpServers": {
    "bpm-engine": {
      "command": "cargo",
      "args": ["run", "--release", "-p", "bpm-engine-mcp-server"],
      "env": {
        "BPM_BASE_URL": "http://127.0.0.1:3000",
        "BPM_MCP_TOKEN": "<minted JWT>",
        "BPM_MCP_ROLE": "operator",
        "BPM_MCP_TIMEOUT_MS": "10000"
      }
    }
  }
}
```

| Field | Value | Notes |
|---|---|---|
| `command` / `args` | `cargo run --release -p bpm-engine-mcp-server` | Run from a checkout of this repo. For a fixed binary, build once and point `command` at `target/release/bpm-engine-mcp-server` directly. |
| `BPM_BASE_URL` | `http://127.0.0.1:3000` | The engine REST base URL. Loopback `http://` is always allowed; a NON-loopback `http://` URL together with a credential is refused at startup (cleartext guard — use `https://` or set `BPM_MCP_ALLOW_INSECURE_HTTP=1` to accept the risk explicitly). |
| `BPM_MCP_TOKEN` | a minted JWT | **Preferred credential.** Mint with the SAME secret the engine verifies: `BPM_JWT_SECRET=… cargo run -p bpm-engine-server-rest --example mint_jwt -- --sub mcp-agent --tenant acme --role operator`. The role claim should match `BPM_MCP_ROLE` so the allowlist and the engine's RBAC agree. |
| `BPM_MCP_ROLE` | `auditor` \| `deployer` \| `operator` \| `worker` | Tool allowlist role preset. Leave unset for the read-only default surface. Alternative: `BPM_MCP_ALLOWED_TOOLS` (explicit comma-separated list, intersected with the preset when both are set). |
| `BPM_MCP_TIMEOUT_MS` | `10000` (default) | Per-call engine REST timeout. |
| (omit all credentials) | — | Open mode: works only against an engine in open mode (local development). |
| `BPM_MCP_API_KEY` | legacy admin key | The `X-API-Key` bootstrap path (full engine access); `BPM_MCP_TOKEN` wins when both are set. |

Common MCP wiring failures (allowlist typos, cleartext guard, wrong-secret
401s) are covered in `docs/troubleshooting.md`.
