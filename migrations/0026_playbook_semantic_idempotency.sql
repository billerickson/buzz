-- Preserve full semantic command payloads for wrapper-independent idempotency.
--
-- 0025 was exercised on pre-PR development databases before this contract was
-- finalized. Keep that checksum stable and add these columns here for both
-- fresh installs and those disposable development databases.

ALTER TABLE playbook_item_actions
    ADD COLUMN IF NOT EXISTS command_payload JSONB;

UPDATE playbook_item_actions
SET command_payload = jsonb_build_object(
    'legacy_event_id',
    encode(event_id, 'hex')
)
WHERE command_payload IS NULL;

ALTER TABLE playbook_item_actions
    ALTER COLUMN command_payload SET NOT NULL;

ALTER TABLE playbook_structure_operations
    ADD COLUMN IF NOT EXISTS command_payload JSONB;

UPDATE playbook_structure_operations
SET command_payload = jsonb_build_object(
    'legacy_event_id',
    encode(event_id, 'hex')
)
WHERE command_payload IS NULL;

ALTER TABLE playbook_structure_operations
    ALTER COLUMN command_payload SET NOT NULL;
