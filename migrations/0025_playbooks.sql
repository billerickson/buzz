-- Native Playbooks current-state projections and append-only audit rows.

CREATE TABLE playbook_templates (
    community_id UUID NOT NULL REFERENCES communities(id),
    template_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'archived', 'deleted')),
    snapshot JSONB NOT NULL,
    last_event_id BYTEA NOT NULL,
    last_actor BYTEA NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, template_id)
);

CREATE TABLE playbook_template_revisions (
    community_id UUID NOT NULL REFERENCES communities(id),
    template_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    snapshot JSONB NOT NULL,
    event_id BYTEA NOT NULL,
    actor BYTEA NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, template_id, revision),
    UNIQUE (community_id, event_id)
);

CREATE TABLE playbook_instances (
    community_id UUID NOT NULL REFERENCES communities(id),
    instance_id UUID NOT NULL,
    channel_id UUID NOT NULL,
    source_template_id UUID NOT NULL,
    source_template_revision BIGINT NOT NULL CHECK (source_template_revision > 0),
    structure_revision BIGINT NOT NULL CHECK (structure_revision > 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'archived')),
    snapshot JSONB NOT NULL,
    created_event_id BYTEA NOT NULL,
    created_by BYTEA NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, instance_id),
    UNIQUE (community_id, created_event_id),
    FOREIGN KEY (community_id, channel_id)
        REFERENCES channels (community_id, id),
    FOREIGN KEY (community_id, source_template_id)
        REFERENCES playbook_templates (community_id, template_id)
);

CREATE UNIQUE INDEX idx_playbook_instances_one_active_per_channel
    ON playbook_instances (community_id, channel_id)
    WHERE status = 'active';

CREATE INDEX idx_playbook_instances_template
    ON playbook_instances (community_id, source_template_id);

CREATE TABLE playbook_item_actions (
    community_id UUID NOT NULL REFERENCES communities(id),
    action_id UUID NOT NULL,
    instance_id UUID NOT NULL,
    item_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('complete', 'reopen')),
    event_id BYTEA NOT NULL,
    actor BYTEA NOT NULL,
    client_created_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, action_id),
    UNIQUE (community_id, event_id),
    FOREIGN KEY (community_id, instance_id)
        REFERENCES playbook_instances (community_id, instance_id)
);

CREATE INDEX idx_playbook_item_actions_instance
    ON playbook_item_actions (community_id, instance_id, accepted_at, event_id);

CREATE TABLE playbook_structure_operations (
    community_id UUID NOT NULL REFERENCES communities(id),
    operation_id UUID NOT NULL,
    instance_id UUID NOT NULL,
    base_revision BIGINT NOT NULL CHECK (base_revision > 0),
    result_revision BIGINT NOT NULL CHECK (result_revision > base_revision),
    operation_type TEXT NOT NULL,
    event_id BYTEA NOT NULL,
    actor BYTEA NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, operation_id),
    UNIQUE (community_id, event_id),
    FOREIGN KEY (community_id, instance_id)
        REFERENCES playbook_instances (community_id, instance_id)
);

CREATE INDEX idx_playbook_structure_operations_instance
    ON playbook_structure_operations (community_id, instance_id, accepted_at, event_id);
