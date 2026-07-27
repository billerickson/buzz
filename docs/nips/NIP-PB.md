NIP-PB
======

Native Playbooks
----------------

`draft` `optional` `relay`

**Depends on**: NIP-01 (events and filters), NIP-29 (channel scope), NIP-33
(parameterized replacement), NIP-42/NIP-98 (authenticated relay and HTTP
access)

## Abstract

This NIP defines reusable checklist templates and independent channel-scoped
playbook instances. Signed command events are the immutable audit source. The
relay validates permissions and optimistic structural revisions, stores
append-only item actions and structural operations, and publishes relay-signed
current projections through ordinary Nostr queries.

Buzz's HTTP bridge uses the existing `POST /events`, `POST /query`, and
`POST /count` surfaces. There are no playbook-specific REST endpoints or
unsigned mutation paths.

## Event Kinds

| Kind | Author | Scope | Purpose |
|---|---|---|---|
| `40200` | workspace owner/admin | community | Immutable full template revision |
| `40201` | channel owner/admin | channel | Deep-copy an active template into a channel |
| `40202` | current channel member | channel | Complete or reopen one item |
| `40203` | channel owner/admin | channel | Apply one structural operation |
| `30623` | relay only | community | Current template projection |
| `30624` | relay only | channel | Current instance, item-state, and activity projection |

Every JSON payload carries `schema_version`; this document defines version `1`.
Unsupported versions MUST be rejected. Client submissions of projection kinds
MUST be rejected.

## Tags

Tag cardinality is exact: a command MUST contain no duplicate or malformed form
of any required playbook tag.

- Template revision (`40200`): exactly one `["template", "<template-uuid>"]`
  and no `h` tag.
- Instance insert (`40201`): exactly one each of `h`, `instance`, and
  `template`.
- Item action (`40202`): exactly one `h` and one `instance`.
- Structure operation (`40203`): exactly one `h` and one `instance`.
- Template snapshot (`30623`): `d` and `template`, both the template UUID.
- Instance snapshot (`30624`): `d` and `instance`, both the instance UUID,
  plus `h` and the source `template`.

Every tag value MUST agree with its JSON payload. The `d` tag makes projections
parameterized replaceable; the relay identity is their author.

## Command Results

Every accepted Playbooks result exposes both `event_id` and
`canonical_event_id`. `event_id` is always the newly submitted signed wrapper.
On first acceptance, `canonical_event_id` equals `event_id`. On a semantic
retry, `canonical_event_id` identifies the first stored command while
`event_id` still identifies the retry wrapper.

The HTTP bridge and CLI expose both identifiers as top-level result fields and
retain the current projection in the response message. A WebSocket `OK` frame
always echoes the submitted wrapper in `OK[1]`. A newly signed semantic retry
MUST return exactly:

```json
["OK","<wrapper-id>",true,"duplicate: canonical_event_id=<64-lowercase-hex>"]
```

The retry wrapper is acknowledged but is not stored or broadcast.

## Template Revisions

A `40200` payload is a complete `TemplateRevision`: stable `template_id`,
monotonic `revision` beginning at 1, name, optional description, lifecycle
status, and ordered sections/items. Section and item IDs MUST be unique within
the full document. Positions MUST be positive sparse integers.

Revisions are immutable and retained. The next accepted revision MUST be
exactly current revision + 1. An `archived` or `deleted` template is immutable
and cannot be inserted. Deleting a template already referenced by an instance
MUST be rejected; it can be archived instead.

## Channel Instances

A `40201` payload identifies a fresh `instance_id`, target `channel_id`, and
active source `template_id`. The relay resolves the latest active template and
deep-copies its nested content. The instance records the exact source template
ID and revision for provenance; later template changes MUST NOT mutate it.

Buzz MVP permits one active instance per channel. Storage and protocol remain
instance-addressed so that this restriction can be relaxed without a data
migration. Archiving the active instance frees the channel for another insert.

## Item Actions

A `40202` payload contains an `action_id`, `instance_id`, `item_id`,
`action` (`complete` or `reopen`), and advisory `client_created_at`. Only the
signed event author and relay acceptance time are canonical.

Actions are append-only. Current state is selected by canonical acceptance time
and then event ID as a deterministic tie-breaker. The full projection retains
all accepted actions, actor pubkeys, event IDs, canonical times, and advisory
client times. Actions targeting an absent, tombstoned, wrong-channel, or
archived item MUST be rejected.

`action_id` is a semantic idempotency key. A retry by the same actor with the
same channel, target, action, and payload returns the retry wrapper as
`event_id`, the first stored command as `canonical_event_id`, and the current
projection even when the retry has a newly signed wrapper. The retry MUST NOT
be stored or broadcast and MUST NOT add an action or activity row. Reuse by a
different actor or with changed semantic content MUST conflict.

## Structural Operations

A `40203` payload contains `operation_id`, `instance_id`,
`base_structure_revision`, and one tagged operation:

- `section.add`, `section.update`, `section.remove`, `section.restore`,
  `section.reorder`
- `item.add`, `item.update`, `item.remove`, `item.restore`, `item.reorder`,
  `item.duplicate`
- `instance.rename`, `instance.archive`

The relay locks the instance, compares the base revision, applies exactly one
operation, and increments the structural revision atomically. A stale base MUST
return a conflict and leave the instance unchanged. Remove operations are
tombstones. Restore reuses the original ID; duplicate requires a fresh item ID.
Archived instances reject all later mutations.

`operation_id` is a semantic idempotency key. A retry by the same actor with the
same channel, target, base revision, operation, and payload returns the retry
wrapper as `event_id`, the first stored command as `canonical_event_id`, and
the current projection even when the retry has a newly signed wrapper. The
retry MUST NOT be stored or broadcast and MUST NOT add an audit/activity row
or revision. Reuse by a different actor or with changed semantic content MUST
conflict.

## Permissions

Authorization is evaluated by the relay for every command:

| Action | Required current role |
|---|---|
| Create/revise/archive/delete template | workspace owner/admin |
| Insert instance | channel owner/admin |
| Complete/reopen item | current channel member |
| Edit/archive instance | channel owner/admin |

A removed member immediately loses mutation access. Reads use the relay's
ordinary community/channel access gates; an instance snapshot never bypasses
channel visibility.

## Projection and Realtime Behavior

After accepting a command, the relay publishes the original signed command to
eligible subscriptions and replaces the corresponding relay-signed snapshot.
Template snapshots contain the current revision and its canonical editor/time.
Instance snapshots contain the deep-copied structure, latest item states, and
the complete ordered activity log.

If command persistence and projection succeeded but snapshot publication was
interrupted, an exact signed-event retry MUST compare the durable projection
with the current snapshot and publish only when repair is needed. A semantic
retry in a new wrapper returns the canonical command result without storing or
broadcasting the retry wrapper. Neither retry form may emit a second identical
current-state event.

Clients rebuild state after reconnect by querying `30623`/`30624` snapshots and
then resuming live subscriptions. They MUST verify event IDs and signatures and
MUST treat the relay signer as the projection authority.

## Errors

Malformed payloads, invalid references, unsupported schemas, and tag mismatches
are validation errors. Missing permissions are authorization errors. Stale
structural revisions and reused idempotency identifiers are conflicts. Buzz's
HTTP bridge maps conflicts to HTTP `409`; the CLI maps them to its conflict exit
variant.

## Privacy and Observability

General telemetry may include community/channel/instance identifiers, event ID,
actor, operation type, outcome, and relay time. It MUST NOT copy checklist text
or template descriptions into metrics or general-purpose logs. Private content
remains in access-scoped events and projections.

## Compatibility

NIP-PB does not reinterpret Canvas Markdown or change `canvas_template`.
Unsupported clients ignore the custom kinds. Existing instances never
subscribe to or synchronize with later template revisions.
