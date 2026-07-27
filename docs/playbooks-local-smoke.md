# Playbooks Phase 1 local smoke test

This runbook exercises the branch-built relay and CLI against a disposable
local community. It refuses to target anything except
`http://127.0.0.1:3000`, unsets `BUZZ_AUTH_TAG`, and uses Buzz's public Tyler
development identity. Never use that identity outside local development.

## 1. Build and start the isolated dev stack

From the Phase 1 worktree root:

```bash
. ./bin/activate-hermit
just setup

export BUZZ_BIND_ADDR=127.0.0.1:3000
export RELAY_URL=ws://127.0.0.1:3000
export BUZZ_RELAY_URL=ws://127.0.0.1:3000
export RELAY_OWNER_PUBKEY=e5ebc6cdb579be112e336cc319b5989b4bb6af11786ea90dbe52b5f08d741b34
unset BUZZ_AUTH_TAG BUZZ_PRIVATE_KEY

just dev
```

The relay owner pubkey belongs to the public local-only Tyler fixture. Setting
it before relay startup gives the fixture permission to create community-wide
templates. The worktree-specific desktop app and keyring remain separate from
the installed production app.

## 2. Run the CLI acceptance smoke test

Keep `just dev` running. In a second terminal:

```bash
# First change to the same Phase 1 worktree.
. ./bin/activate-hermit
cargo build -p buzz-cli

export BUZZ_RELAY_URL=http://127.0.0.1:3000
./scripts/playbooks-smoke.sh
```

The script creates unique fixture IDs on every run, then verifies:

- template create, get, list-compatible projection, and revision update;
- insertion into an existing channel and source-revision provenance;
- complete and reopen state with append-only activity;
- newly signed semantic retries for item and structural commands, including
  canonical event-ID reuse and no duplicate command, activity row, item, or
  structural revision;
- an item-level structural edit and structure revision increment;
- template-to-instance deep-copy isolation after the template changes; and
- normalized create envelopes containing `template_id` and `instance_id`.

Success ends with `Playbooks Phase 1 smoke test passed` and prints only the
disposable channel/template/instance/item IDs. It never prints a private key.
