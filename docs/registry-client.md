> **Current-interface reference, not the target specification.** This document describes the implemented interface at baseline `0a9c551`. Future requirements and their delivery status live in [specs/README.md](specs/README.md).

# Official registry client contract

Dust 1.0 recognizes one registry base:
`https://dust.blurskye.dev/registry/v1`. The client never follows redirects,
uses no environment proxy, and rejects URLs escaping that exact HTTPS
scheme/host/port/path prefix. Configurable mirrors are post-1.0.

## Endpoints consumed

- `GET packages/search?q=QUERY&limit=N`, `Accept: application/json`
- `GET packages/PUBLISHER/NAME/VERSION`, `Accept: application/json`
- `GET packages/PUBLISHER/NAME/VERSION/artifacts/FILE`,
  `Accept: application/wasm`

Search and exact metadata use schema `1`. Exact metadata carries publisher,
name, immutable version, yanked state, an artifact path under that identity,
artifact SHA-256, the typed package manifest, and the typed plugin capability
manifest. Package identity must be `publisher/name`; all duplicated
version/digest/source fields must agree. Yanked exact metadata is rejected.

The client checks status and exact media type, rejects redirects, rejects malformed or multiple Content-Length fields, checks a single valid present length before reading, and always reads through a
`maximum + 1` streaming ceiling. Artifact SHA-256 is verified before package
preparation. Metadata and artifact deadlines and ceilings are independent.

## Cache and offline behavior

Bodies use a SHA-256 URL key under the user package registry cache. Each body
has a separate SHA-256 integrity sidecar. Both are regular user-only files;
unsafe cache directories, symlinks, missing sidecars, changed bytes, or an
overbudget cache fail closed. Both body and checksum destinations are fail-closed preflighted before either replacement. Writes use distinct create-new quarantine files, file sync, atomic rename, and parent-directory sync. Pruning is bounded by configured entry and byte ceilings.
Offline mode performs no request and returns an explicit cache miss unless the
integrity-checked object exists.

## Activation transaction

A fetched component enters the immutable store and production Component host
validation first. The package worker returns an in-memory prepared token but
does not modify the lock. The application asks the persistent plugin owner to
load it. Only a `Loaded` event causes a token commit and atomic lock switch.
Failure or process exit before commit leaves the previous lock active.
Committed activation manifests are checksum-bound to lock objects and replayed at startup. Immediate and asynchronous startup/reconciliation load failures conditionally unload their exact runtime generations and attempt the validated retained previous lock once. A late event from a cancelled load cannot acknowledge a prepared token or disable a replacement load with the same plugin ID. Queue-full cancellation remains foreground-inactive and pending until its exact conditional disable is ordered to the worker; same-ID replacement waits behind it. The validated plugin queue capacity bounds current loads and pending disables. Worker loss aborts prepared tokens and removes runtime command ownership; it never converts a missing acknowledgment into a lock commit. Explicit remove/rollback starts a fresh one-shot reconciliation attempt; failure to queue rollback is reported rather than represented as success.

## Explicitly absent

This client does not implement registry-server namespace enrollment,
publisher signing, moderation, advisory generation, or artifact publication.
Those require the separately operated service policy. Direct Git network
transport is also not implemented and never shells out to a Git CLI.
