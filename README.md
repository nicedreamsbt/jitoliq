## jitoliq (Jito bundles send demo)

This repo is a **minimal, standalone demo** of how we submit bundles to the **Jito Block Engine** using **JSON-RPC over HTTP**.

It is intentionally focused on the transport + anti-spam mechanics Jito asked to review:

- **JSON-RPC methods**: `getTipAccounts`, `sendBundle`, `getBundleStatuses`
- **Rate limiting / throttling knobs** (env-configurable)
- **Retry/backoff** for `429` and `5xx`
- **Endpoint fallback** across multiple Block Engine URLs
- **Encoding fallback**: try **base64** first; on decode rejection, retry with **base58**

### Production snippets (real code)

In addition to the runnable demo crate, we also included **read-only excerpts** from our production bot to make review easier:

- `prod_snippets/eva01_src_jito_bundle.rs`
  - Our real `JitoBundleClient` implementation (JSON-RPC over HTTP via reqwest)
  - Throttling knobs + 429/5xx retry + endpoint fallback
  - Base64 → base58 retry on decode errors
  - Optional `getBundleStatuses` polling
- `prod_snippets/eva01_src_liquidator_bundle_callsite.rs`
  - Real call-site snippet showing bundle ordering and the **optional RPC fallback send** after a delay

These snippet files are **not compiled** here; they are provided purely for Jito review.

### What we are sharing (and what we are not)

- **Included**
  - `src/lib.rs`: `JitoBundleClient` (reqwest blocking client)
  - `src/main.rs`: tiny CLI that calls `getTipAccounts` and optionally `sendBundle`
  - `prod_snippets/*`: production excerpts related to bundle/tx submission
- **Not included**
  - No wallet keys, no `.env`, no RPC creds, no production liquidator logic, no strategy code
  - No on-chain program interaction logic; this is purely the **bundle submission pipeline**

### Run

Set Block Engine URLs (either base host or full `/api/v1/bundles` URL):

```bash
export JITO_BLOCK_ENGINE_URLS="https://frankfurt.mainnet.block-engine.jito.wtf,https://ny.mainnet.block-engine.jito.wtf"
```

Run the demo:

```bash
cargo run
```

Optional knobs:

- `JITO_SEND_BUNDLE_MIN_INTERVAL_MS` (default `0`)
- `JITO_TIP_ACCOUNTS_MIN_INTERVAL_MS` (default `1200`)
- `JITO_OTHER_MIN_INTERVAL_MS` (default `250`)

Optional: submit a bundle by providing tx bytes (bincode) as base64 strings:

- `BUNDLE_TXS_BASE64_JSON='["...","..."]'`

### Notes

- This demo uses **JSON-RPC** (not gRPC).
- The public `jitoliq` GitHub repo is currently empty; you can push this crate as the initial commit:
  - [nicedreamsbt/jitoliq](https://github.com/nicedreamsbt/jitoliq)


