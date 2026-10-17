# Proposal: optional per-client-key OpenAI installation identities

Status: proposed behavior for upstream review; implementation is pending

## Purpose and source basis

Requests routed through the same OpenAI account currently use that account's
installation ID, including when client keys select independent UA profiles.
This proposal lets a client key opt into a separate installation identity for
each account it uses, while retaining the account identity by default.

The existing OpenAI provider applies the selected account's ID through
[`scope_request_to_account`](https://github.com/zyycn/codex-proxy-rs/blob/83f5d71853f90ea4006a38bee6c916e56844a557/backend/crates/providers/openai/src/provider/mod.rs#L883).
Official Codex [`rust-v0.160.1`](https://github.com/openai/codex/blob/d27764b82f7118f674371e6d6e76271d9d606edb/codex-rs/core/src/installation_id.rs#L19)
reuses a persisted UUID and generates UUIDv4 when an installation ID is needed.
The proposal follows that generate-once-and-persist behavior. Codex scopes its ID
to `CODEX_HOME`; scoping it to a client-key/account pair is a proxy-specific choice.

## Configuration and identity

- Offer an off-by-default toggle when a client key explicitly selects an independent UA profile
- Determine eligibility from the inheritance setting, without comparing UA strings
- Resolve account identity mode when the key inherits the global profile or the toggle is off
- Persist the option with the key's OpenAI settings and preserve it through editing, preview, and runtime resolution
- Key each installation mapping by the stable client-key and provider-account record IDs
- Exclude UA values and key secrets from identity selection
- Preserve the pair's UUID across UA changes, process restarts, and disabling/re-enabling the toggle

The UI exposes the choice; configuration parsing must also enforce its scope so
API updates have the same behavior. Returning to global inheritance selects the
account identity mode.

## Persistence and request flow

Add a normal upstream migration for an `openai_client_installations` table:

| Column | Meaning |
| --- | --- |
| `client_api_key_id` | Stable client-key record ID |
| `provider_account_id` | Stable account record ID |
| `installation_id` | Persisted UUIDv4 |

Use `(client_api_key_id, provider_account_id)` as the primary key. Both parent
references use foreign keys with `ON DELETE CASCADE`.

Resolve the mode from the current execution's key configuration. After account
selection, select the installation ID as follows:

1. In account mode, immediately use the ID already available on the account lease
2. In per-key mode, look up the pair in the in-memory UUID cache
3. On a cache miss, read the persisted mapping
4. If the row is absent, generate UUIDv4 and insert it without overwriting an existing row
5. If another caller wins the insert, read and use that caller's persisted UUID
6. Cache and return only a successfully persisted UUID

The composite primary key arbitrates creation across processes. Concurrent
initialization for the same pair within a process should share one operation.
Database errors remain errors rather than silently substituting the account ID;
a retry can load any mapping that was committed before a response was lost.

New keys and accounts need no eager initialization. Their first request in
per-key mode creates the missing pair. Deleting either parent removes its rows.

The OpenAI provider owns identity selection and the cache. Store implements
mapping persistence through a provider-owned port. The existing Admin/Core
configuration and snapshot paths carry the option, and transport applies the
selected ID through the existing metadata-scoping helpers.

## Cache and configuration lifecycle

Keep a bounded cache of persisted UUID mappings. Account mode bypasses this
cache and the installation table entirely, regardless of whether a mapping
already exists. The existing account credential-loading work remains unchanged.

Enabled/disabled status belongs to key configuration, not to the UUID cache.
Key edits already
[publish a committed configuration revision](https://github.com/zyycn/codex-proxy-rs/blob/83f5d71853f90ea4006a38bee6c916e56844a557/backend/crates/gateway-admin/src/use_case/client_keys.rs#L220),
and downstream WebSocket requests
[refresh policy before each execution](https://github.com/zyycn/codex-proxy-rs/blob/83f5d71853f90ea4006a38bee6c916e56844a557/backend/crates/gateway-api/src/openai/responses/websocket/mod.rs#L284).
Carry the option through those existing paths. Each new execution uses its
current snapshot; an execution already underway keeps its captured settings.

| Setting or cache change | Result |
| --- | --- |
| Disabled to enabled | Resolve the pair from cache or storage, creating it if absent |
| Enabled to disabled | Use the account ID and bypass UUID resolution |
| Disabled to enabled again | Reuse the pair's persisted UUID |
| Independent UA changes while enabled | Keep the same pair UUID |
| Cache eviction or process restart | Reload the persisted UUID |

The cache stores successful UUID results and transient initialization work only.
It stores no disabled or negative result. UUID mappings need no time-based
expiration because they are immutable; configuration refresh handles policy
changes independently.

## Request metadata and connection behavior

Pass the selected ID through the existing scoping helpers so request-body
installation metadata and the serialized turn-metadata header agree wherever
they are emitted. Apply the same selection to supported OpenAI request paths.

No conversation identity tracking or forced upstream WebSocket reconnection is
proposed. An existing upstream WebSocket retains its opening handshake headers
for the lifetime of that connection. Subsequent executions use the refreshed
configuration for newly constructed request metadata; a new handshake uses the
then-current identity.

## Implementation acceptance criteria

- Disabled keys bypass UUID cache/store access, including when a pair mapping already exists
- Key edits take effect after normal snapshot refresh without restarting, including subsequent downstream WebSocket executions
- Different keys and accounts receive distinct, stable UUIDv4 mappings
- Newly created keys and accounts initialize mappings on first use in per-key mode
- Concurrent creation returns the same persisted identity within and across processes
- Cache eviction, restart, UA edits, and disabling/re-enabling preserve the pair's identity
- Storage failures and cancelled initialization do not cache an unpersisted UUID or silently change identity mode
- Request-body and turn-metadata identity agree for HTTP and newly opened upstream WebSocket connections
- Deleting either parent removes its persisted mappings
- UI editing and API round trips preserve the option and enforce explicit inheritance semantics

These are planned implementation checks, not completed test results.
