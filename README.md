## jitoliq (Jito bundles send demo)

This repo is a **minimal, standalone demo** of how we submit bundles to the **Jito Block Engine** using **JSON-RPC over HTTP**.

It is intentionally focused on the transport + anti-spam mechanics Jito asked to review:

- **JSON-RPC methods**: `getTipAccounts`, `sendBundle`, `getBundleStatuses`
- **Rate limiting / throttling knobs** (env-configurable)
- **Retry/backoff** for `429` and `5xx`
- **Endpoint fallback** across multiple Block Engine URLs
- **Encoding fallback**: try **base64** first; on decode rejection, retry with **base58**

### What we are sharing (and what we are not)

- **Included**
  - `src/lib.rs`: `JitoBundleClient` (reqwest blocking client)
  - `src/main.rs`: tiny CLI that calls `getTipAccounts` and optionally `sendBundle`
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


