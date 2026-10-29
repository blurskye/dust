# Official registry client contract

The checked-out client recognizes one registry base:
`https://dust.blurskye.dev/registry/v1`. The client never follows redirects,
uses no environment proxy, and rejects URLs escaping that exact HTTPS
scheme/host/port/path prefix. Configurable mirrors are unsupported. This is an
implemented client contract, not evidence that a public registry service or
publisher programme is deployed or reachable.

The current API manifest reports `package_network = official-https-no-redirect-v1`.
In-app `package.registry.search` and `.show` are explicit network actions;
`package.registry.install` additionally requires the workspace to be trusted and is
blocked by safe mode. The bundled trust prompt requires **`trust this workspace`**,
but current configuration schema 1 can replace that phrase; this is a known Partial
mismatch with the protected target copy, not registry or plugin authority to bypass
trust. The application has no automatic startup update.

## Endpoints consumed

- `GET packages/search?q=QUERY&limit=N`, `Accept: application/json`
- `GET packages/PUBLISHER/NAME/VERSION`, `Accept: application/json`
- `GET packages/PUBLISHER/NAME/VERSION/artifacts/FILE`,
  `Accept: application/wasm`

Search and exact metadata use schema `1`. Exact metadata carries publisher,
name, version, yanked state, an artifact path under that identity, artifact
SHA-256, the typed package manifest, and the typed plugin capability manifest.
Package identity must be `publisher/name`; all duplicated version/digest/source
fields must agree. Yanked exact metadata is rejected. Publisher/name segments
are 1–128 ASCII alphanumeric, dash or underscore bytes; version is 1–128 ASCII
alphanumeric, dot, dash or plus bytes. The client does not parse SemVer. Search
queries are nonempty, control-free and at most 256 bytes; a schema-1 response
has at most the configured result count and each control-free description is at
most 4,096 bytes. Exact artifact paths must remain under
`packages/PUBLISHER/NAME/VERSION/artifacts/` on the official origin.

The client checks status and exact media type, rejects redirects, rejects malformed or multiple Content-Length fields, checks a single valid present length before reading, and always reads through a
`maximum + 1` streaming ceiling. Artifact SHA-256 is verified before package
preparation. One configured connect timeout and one configured whole-request
timeout apply to every endpoint; metadata and artifact byte ceilings are
separate. Submission can fail before admission with queue-full or worker-stopped;
worker results distinguish policy/origin, redirect/status/content-type/content-length,
oversize, invalid/yanked metadata, checksum, offline miss, unsafe cache,
HTTP, I/O and thread failures. None of those failures publishes an install
candidate.

## Current configuration bounds

`[packages.registry]` is strict configuration. The fixed origin and zero
redirect limit cannot be relaxed; other accepted values and bundled defaults
are:

| Setting | Default | Accepted range |
|---|---:|---:|
| `offline` | `false` | Boolean |
| `connect_timeout_ms` | 5,000 | 100–30,000 |
| `request_timeout_ms` | 30,000 | 100–120,000 |
| `maximum_metadata_bytes` | 1 MiB | 1 KiB–8 MiB |
| `maximum_artifact_bytes` | 64 MiB | 1 KiB–256 MiB |
| `maximum_cache_bytes` | 512 MiB | Artifact ceiling–16 GiB |
| `maximum_cache_entries` | 4,096 | 1–100,000 |
| `maximum_search_results` | 100 | 1–1,000 |
| `queue_capacity` | 8 | 1–64 |

These are current configurable client bounds, not the target defaults or a
claim that the target C-06 maximums are used in production.

## Cache and offline behavior

Bodies use a SHA-256 URL key under the user package registry cache. Each body
has a separate SHA-256 integrity sidecar. Both must be regular files; the cache
directory and newly written files receive owner-only modes on Unix. Unsafe
cache directories, symlinks, missing sidecars, changed bytes, or an overbudget
cache fail closed. Both body and checksum destinations are fail-closed preflighted before either replacement. Writes use distinct create-new quarantine files, file sync, atomic rename, and parent-directory sync. Pruning enforces configured entry and byte ceilings.
Offline mode performs no request and returns an explicit cache miss unless the
integrity-checked object exists.

## Activation transaction

Registry schema 1 embeds the current JSON package descriptor and schema-1
plugin TOML model; it does not carry or imply `dust-package.toml` schema 2.
Exact current records and lock version 1 are documented in the
[current SDK](plugin-sdk.md#current-package-and-lock-formats).

A fetched component enters the immutable store and production 0.2 Component
host validation first. The package worker returns an in-memory prepared token
but does not modify the lock. The application asks the persistent plugin owner
to load it. Only a `Loaded` event causes a token commit and atomic lock switch.
Failure or process exit before commit leaves the previous lock active.
Committed activation manifests are checksum-bound to lock objects and replayed at startup. Immediate and asynchronous startup/reconciliation load failures conditionally unload their exact runtime generations and attempt the validated retained previous lock once. A late event from a cancelled load cannot acknowledge a prepared token or disable a replacement load with the same plugin ID. Queue-full cancellation remains foreground-inactive and pending until its exact conditional disable is ordered to the worker; same-ID replacement waits behind it. The validated plugin queue capacity bounds current loads and pending disables. Worker loss aborts prepared tokens and removes runtime command ownership; it never converts a missing acknowledgment into a lock commit. Explicit remove/rollback starts a fresh one-shot reconciliation attempt; failure to queue rollback is reported rather than represented as success.

## Explicitly absent

This client does not implement registry-server namespace enrollment,
publisher signing, moderation, advisory generation, artifact publication,
mirrors, background updates or authenticated advisory/yank feeds. SHA-256
proves downloaded-byte integrity against the received metadata; without an
authenticated publisher/service signature it does not prove publisher identity.
Direct Git network transport is also not implemented and never shells out to a
Git CLI. The target 0.3 package, permission/grant, provider/service and release
contracts remain unavailable through this schema-1 client.
