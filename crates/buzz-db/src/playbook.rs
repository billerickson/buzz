//! Atomic Postgres projections for native Buzz Playbooks.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use buzz_core::playbook::{
    fold_item_states, ActivityEntry, InstanceInsert, InstanceSnapshot, InstanceStatus, ItemAction,
    ItemActionKind, ItemState, PlaybookInstance, StructureOperation, TemplateRevision,
    TemplateSnapshot, TemplateStatus,
};
use buzz_core::CommunityId;

use crate::{DbError, Result};

/// Result of applying a semantically idempotent Playbooks command.
#[derive(Debug)]
pub struct SemanticApply<T> {
    /// Current projection after the canonical command.
    pub snapshot: T,
    /// Signed event ID retained as the canonical command.
    pub canonical_event_id: Vec<u8>,
    /// Whether this call created the canonical command.
    pub applied: bool,
}

fn template_status(status: TemplateStatus) -> &'static str {
    match status {
        TemplateStatus::Active => "active",
        TemplateStatus::Archived => "archived",
        TemplateStatus::Deleted => "deleted",
    }
}

fn instance_status(status: InstanceStatus) -> &'static str {
    match status {
        InstanceStatus::Active => "active",
        InstanceStatus::Archived => "archived",
    }
}

fn item_action_name(action: ItemActionKind) -> &'static str {
    match action {
        ItemActionKind::Complete => "complete",
        ItemActionKind::Reopen => "reopen",
    }
}

async fn require_workspace_admin(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    actor: &[u8],
) -> Result<()> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role::text FROM relay_members \
         WHERE community_id = $1 AND pubkey = $2 \
         FOR SHARE",
    )
    .bind(community_id.as_uuid())
    .bind(hex::encode(actor))
    .fetch_optional(tx.as_mut())
    .await?;
    if matches!(role.as_deref(), Some("owner" | "admin")) {
        Ok(())
    } else {
        Err(DbError::AccessDenied(
            "workspace owner/admin permission required".into(),
        ))
    }
}

async fn require_channel_role(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    channel_id: Uuid,
    actor: &[u8],
    elevated: bool,
) -> Result<String> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT cm.role::text FROM channel_members cm \
         JOIN channels c ON c.community_id = cm.community_id AND c.id = cm.channel_id \
         WHERE cm.community_id = $1 AND cm.channel_id = $2 AND cm.pubkey = $3 \
           AND cm.removed_at IS NULL AND c.deleted_at IS NULL AND c.archived_at IS NULL \
         FOR SHARE OF cm",
    )
    .bind(community_id.as_uuid())
    .bind(channel_id)
    .bind(actor)
    .fetch_optional(tx.as_mut())
    .await?;
    let role =
        role.ok_or_else(|| DbError::AccessDenied("current channel membership required".into()))?;
    if elevated && !matches!(role.as_str(), "owner" | "admin") {
        return Err(DbError::AccessDenied(
            "channel owner/admin permission required".into(),
        ));
    }
    Ok(role)
}

/// Apply one immutable full template revision.
pub async fn apply_template_revision(
    pool: &PgPool,
    community_id: CommunityId,
    revision: &TemplateRevision,
    event_id: &[u8],
    actor: &[u8],
) -> Result<TemplateSnapshot> {
    revision
        .validate()
        .map_err(|error| DbError::InvalidData(error.to_string()))?;
    let mut tx = pool.begin().await?;
    require_workspace_admin(&mut tx, community_id, actor).await?;

    if let Some(row) = sqlx::query(
        "SELECT event_id FROM playbook_template_revisions \
         WHERE community_id = $1 AND template_id = $2 AND revision = $3",
    )
    .bind(community_id.as_uuid())
    .bind(revision.template_id)
    .bind(revision.revision as i64)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let existing: Vec<u8> = row.try_get("event_id")?;
        if existing != event_id {
            return Err(DbError::Conflict(format!(
                "template revision {} already exists",
                revision.revision
            )));
        }
        tx.commit().await?;
        return template_snapshot(pool, community_id, revision.template_id).await;
    }

    let current: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision, status FROM playbook_templates \
         WHERE community_id = $1 AND template_id = $2 FOR UPDATE",
    )
    .bind(community_id.as_uuid())
    .bind(revision.template_id)
    .fetch_optional(tx.as_mut())
    .await?;
    let expected = current.as_ref().map_or(1, |(value, _)| value + 1);
    if revision.revision as i64 != expected {
        return Err(DbError::Conflict(format!(
            "template revision conflict: expected {expected}, got {}",
            revision.revision
        )));
    }
    if let Some((_, status)) = current.as_ref() {
        if status == "deleted" {
            return Err(DbError::Conflict("deleted template is immutable".into()));
        }
        if status == "archived" {
            return Err(DbError::Conflict("archived template is immutable".into()));
        }
    }
    if revision.status == TemplateStatus::Deleted {
        let referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM playbook_instances \
             WHERE community_id = $1 AND source_template_id = $2)",
        )
        .bind(community_id.as_uuid())
        .bind(revision.template_id)
        .fetch_one(tx.as_mut())
        .await?;
        if referenced {
            return Err(DbError::Conflict(
                "referenced templates must be archived instead of deleted".into(),
            ));
        }
    }

    let snapshot = serde_json::to_value(revision)?;
    let accepted_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO playbook_template_revisions \
         (community_id, template_id, revision, snapshot, event_id, actor) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING accepted_at",
    )
    .bind(community_id.as_uuid())
    .bind(revision.template_id)
    .bind(revision.revision as i64)
    .bind(&snapshot)
    .bind(event_id)
    .bind(actor)
    .fetch_one(tx.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO playbook_templates \
         (community_id, template_id, revision, status, snapshot, last_event_id, last_actor, accepted_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (community_id, template_id) DO UPDATE SET \
           revision = EXCLUDED.revision, status = EXCLUDED.status, snapshot = EXCLUDED.snapshot, \
           last_event_id = EXCLUDED.last_event_id, last_actor = EXCLUDED.last_actor, \
           accepted_at = EXCLUDED.accepted_at",
    )
    .bind(community_id.as_uuid())
    .bind(revision.template_id)
    .bind(revision.revision as i64)
    .bind(template_status(revision.status))
    .bind(snapshot)
    .bind(event_id)
    .bind(actor)
    .bind(accepted_at)
    .execute(tx.as_mut())
    .await?;
    tx.commit().await?;
    template_snapshot(pool, community_id, revision.template_id).await
}

/// Deep-copy the latest active template revision into a channel.
pub async fn insert_instance(
    pool: &PgPool,
    community_id: CommunityId,
    request: &InstanceInsert,
    event_id: &[u8],
    actor: &[u8],
) -> Result<InstanceSnapshot> {
    if request.schema_version != buzz_core::playbook::PLAYBOOK_SCHEMA_VERSION {
        return Err(DbError::InvalidData(format!(
            "unsupported schema_version {}",
            request.schema_version
        )));
    }
    let mut tx = pool.begin().await?;
    require_channel_role(&mut tx, community_id, request.channel_id, actor, true).await?;

    if let Some(row) = sqlx::query(
        "SELECT created_event_id FROM playbook_instances \
         WHERE community_id = $1 AND instance_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(request.instance_id)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let existing: Vec<u8> = row.try_get("created_event_id")?;
        if existing != event_id {
            return Err(DbError::Conflict("instance_id already exists".into()));
        }
        tx.commit().await?;
        return instance_snapshot(pool, community_id, request.instance_id).await;
    }

    let source_json: serde_json::Value = sqlx::query_scalar(
        "SELECT snapshot FROM playbook_templates \
         WHERE community_id = $1 AND template_id = $2 AND status = 'active' FOR SHARE",
    )
    .bind(community_id.as_uuid())
    .bind(request.template_id)
    .fetch_optional(tx.as_mut())
    .await?
    .ok_or_else(|| DbError::NotFound("active playbook template".into()))?;
    let source: TemplateRevision = serde_json::from_value(source_json)?;
    let instance =
        PlaybookInstance::from_template(request.instance_id, request.channel_id, &source);
    let snapshot = serde_json::to_value(&instance)?;
    let insert = sqlx::query(
        "INSERT INTO playbook_instances \
         (community_id, instance_id, channel_id, source_template_id, source_template_revision, \
          structure_revision, status, snapshot, created_event_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'active', $7, $8, $9)",
    )
    .bind(community_id.as_uuid())
    .bind(request.instance_id)
    .bind(request.channel_id)
    .bind(request.template_id)
    .bind(source.revision as i64)
    .bind(instance.structure_revision as i64)
    .bind(snapshot)
    .bind(event_id)
    .bind(actor)
    .execute(tx.as_mut())
    .await;
    if let Err(error) = insert {
        if error
            .as_database_error()
            .is_some_and(|db| db.is_unique_violation())
        {
            return Err(DbError::Conflict(
                "channel already has an active playbook".into(),
            ));
        }
        return Err(error.into());
    }
    tx.commit().await?;
    instance_snapshot(pool, community_id, request.instance_id).await
}

/// Apply an idempotent member-authored complete/reopen action.
pub async fn apply_item_action(
    pool: &PgPool,
    community_id: CommunityId,
    channel_id: Uuid,
    action: &ItemAction,
    event_id: &[u8],
    actor: &[u8],
) -> Result<SemanticApply<InstanceSnapshot>> {
    if action.schema_version != buzz_core::playbook::PLAYBOOK_SCHEMA_VERSION {
        return Err(DbError::InvalidData(format!(
            "unsupported schema_version {}",
            action.schema_version
        )));
    }
    let mut tx = pool.begin().await?;
    require_channel_role(&mut tx, community_id, channel_id, actor, false).await?;
    let lock_key = format!(
        "playbook-item-action:{}:{}",
        community_id.as_uuid(),
        action.action_id
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(tx.as_mut())
        .await?;
    let payload = serde_json::to_value(action)?;

    if let Some(row) = sqlx::query(
        "SELECT pia.event_id, pia.actor, pia.command_payload, pi.channel_id \
         FROM playbook_item_actions pia \
         JOIN playbook_instances pi \
           ON pi.community_id = pia.community_id AND pi.instance_id = pia.instance_id \
         WHERE pia.community_id = $1 AND pia.action_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(action.action_id)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let canonical_event_id: Vec<u8> = row.try_get("event_id")?;
        let same = row.try_get::<Vec<u8>, _>("actor")? == actor
            && row.try_get::<Uuid, _>("channel_id")? == channel_id
            && row.try_get::<serde_json::Value, _>("command_payload")? == payload;
        if !same {
            return Err(DbError::Conflict(
                "action_id was already used for a different action".into(),
            ));
        }
        tx.commit().await?;
        return Ok(SemanticApply {
            snapshot: instance_snapshot(pool, community_id, action.instance_id).await?,
            canonical_event_id,
            applied: false,
        });
    }

    let snapshot_json: serde_json::Value = sqlx::query_scalar(
        "SELECT snapshot FROM playbook_instances \
         WHERE community_id = $1 AND instance_id = $2 AND channel_id = $3 \
           AND status = 'active' FOR SHARE",
    )
    .bind(community_id.as_uuid())
    .bind(action.instance_id)
    .bind(channel_id)
    .fetch_optional(tx.as_mut())
    .await?
    .ok_or_else(|| DbError::NotFound("active playbook instance".into()))?;
    let instance: PlaybookInstance = serde_json::from_value(snapshot_json)?;
    if !instance.has_active_item(action.item_id) {
        return Err(DbError::NotFound("active playbook item".into()));
    }
    sqlx::query(
        "INSERT INTO playbook_item_actions \
         (community_id, action_id, instance_id, item_id, action, event_id, actor, \
          client_created_at, command_payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(community_id.as_uuid())
    .bind(action.action_id)
    .bind(action.instance_id)
    .bind(action.item_id)
    .bind(item_action_name(action.action))
    .bind(event_id)
    .bind(actor)
    .bind(action.client_created_at)
    .bind(payload)
    .execute(tx.as_mut())
    .await?;
    tx.commit().await?;
    Ok(SemanticApply {
        snapshot: instance_snapshot(pool, community_id, action.instance_id).await?,
        canonical_event_id: event_id.to_vec(),
        applied: true,
    })
}

/// Apply an idempotent structural operation under a row lock.
pub async fn apply_structure_operation(
    pool: &PgPool,
    community_id: CommunityId,
    channel_id: Uuid,
    operation: &StructureOperation,
    event_id: &[u8],
    actor: &[u8],
) -> Result<SemanticApply<InstanceSnapshot>> {
    if operation.schema_version != buzz_core::playbook::PLAYBOOK_SCHEMA_VERSION {
        return Err(DbError::InvalidData(format!(
            "unsupported schema_version {}",
            operation.schema_version
        )));
    }
    let mut tx = pool.begin().await?;
    require_channel_role(&mut tx, community_id, channel_id, actor, true).await?;
    let lock_key = format!(
        "playbook-structure-operation:{}:{}",
        community_id.as_uuid(),
        operation.operation_id
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(tx.as_mut())
        .await?;
    let payload = serde_json::to_value(operation)?;

    if let Some(row) = sqlx::query(
        "SELECT pso.event_id, pso.actor, pso.command_payload, pi.channel_id \
         FROM playbook_structure_operations pso \
         JOIN playbook_instances pi \
           ON pi.community_id = pso.community_id AND pi.instance_id = pso.instance_id \
         WHERE pso.community_id = $1 AND pso.operation_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(operation.operation_id)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let canonical_event_id: Vec<u8> = row.try_get("event_id")?;
        let same = row.try_get::<Vec<u8>, _>("actor")? == actor
            && row.try_get::<Uuid, _>("channel_id")? == channel_id
            && row.try_get::<serde_json::Value, _>("command_payload")? == payload;
        if !same {
            return Err(DbError::Conflict(
                "operation_id was already used for a different operation".into(),
            ));
        }
        tx.commit().await?;
        return Ok(SemanticApply {
            snapshot: instance_snapshot(pool, community_id, operation.instance_id).await?,
            canonical_event_id,
            applied: false,
        });
    }

    let row = sqlx::query(
        "SELECT structure_revision, snapshot FROM playbook_instances \
         WHERE community_id = $1 AND instance_id = $2 AND channel_id = $3 FOR UPDATE",
    )
    .bind(community_id.as_uuid())
    .bind(operation.instance_id)
    .bind(channel_id)
    .fetch_optional(tx.as_mut())
    .await?
    .ok_or_else(|| DbError::NotFound("playbook instance".into()))?;
    let current_revision: i64 = row.try_get("structure_revision")?;
    if operation.base_structure_revision as i64 != current_revision {
        return Err(DbError::Conflict(format!(
            "structure revision conflict: current {current_revision}, base {}",
            operation.base_structure_revision
        )));
    }
    let mut instance: PlaybookInstance = serde_json::from_value(row.try_get("snapshot")?)?;
    instance
        .apply_operation(&operation.operation)
        .map_err(|error| DbError::InvalidData(error.to_string()))?;
    instance.structure_revision += 1;
    let next_revision = instance.structure_revision as i64;
    let snapshot = serde_json::to_value(&instance)?;
    sqlx::query(
        "INSERT INTO playbook_structure_operations \
         (community_id, operation_id, instance_id, base_revision, result_revision, \
          operation_type, event_id, actor, command_payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(community_id.as_uuid())
    .bind(operation.operation_id)
    .bind(operation.instance_id)
    .bind(operation.base_structure_revision as i64)
    .bind(next_revision)
    .bind(operation.operation.name())
    .bind(event_id)
    .bind(actor)
    .bind(payload)
    .execute(tx.as_mut())
    .await?;
    sqlx::query(
        "UPDATE playbook_instances SET structure_revision = $1, status = $2, \
         snapshot = $3, updated_at = NOW() \
         WHERE community_id = $4 AND instance_id = $5",
    )
    .bind(next_revision)
    .bind(instance_status(instance.status))
    .bind(snapshot)
    .bind(community_id.as_uuid())
    .bind(operation.instance_id)
    .execute(tx.as_mut())
    .await?;
    tx.commit().await?;
    Ok(SemanticApply {
        snapshot: instance_snapshot(pool, community_id, operation.instance_id).await?,
        canonical_event_id: event_id.to_vec(),
        applied: true,
    })
}

/// Read the current template projection.
pub async fn template_snapshot(
    pool: &PgPool,
    community_id: CommunityId,
    template_id: Uuid,
) -> Result<TemplateSnapshot> {
    let row = sqlx::query(
        "SELECT snapshot, last_actor, accepted_at, last_event_id \
         FROM playbook_templates WHERE community_id = $1 AND template_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(template_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| DbError::NotFound("playbook template".into()))?;
    Ok(TemplateSnapshot {
        template: serde_json::from_value(row.try_get("snapshot")?)?,
        actor_pubkey: hex::encode(row.try_get::<Vec<u8>, _>("last_actor")?),
        accepted_at: row.try_get("accepted_at")?,
        event_id: hex::encode(row.try_get::<Vec<u8>, _>("last_event_id")?),
    })
}

/// Read an instance, latest item states, and complete activity projection.
pub async fn instance_snapshot(
    pool: &PgPool,
    community_id: CommunityId,
    instance_id: Uuid,
) -> Result<InstanceSnapshot> {
    let row = sqlx::query(
        "SELECT snapshot, created_event_id, created_by, accepted_at \
         FROM playbook_instances WHERE community_id = $1 AND instance_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(instance_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| DbError::NotFound("playbook instance".into()))?;
    let instance: PlaybookInstance = serde_json::from_value(row.try_get("snapshot")?)?;

    let state_rows = sqlx::query(
        "SELECT item_id, action, actor, accepted_at, client_created_at, event_id \
         FROM playbook_item_actions WHERE community_id = $1 AND instance_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(instance_id)
    .fetch_all(pool)
    .await?;
    let item_states = fold_item_states(
        state_rows
            .into_iter()
            .map(|row| {
                Ok(ItemState {
                    item_id: row.try_get("item_id")?,
                    completed: row.try_get::<String, _>("action")? == "complete",
                    actor_pubkey: hex::encode(row.try_get::<Vec<u8>, _>("actor")?),
                    accepted_at: row.try_get("accepted_at")?,
                    client_created_at: Some(row.try_get("client_created_at")?),
                    event_id: hex::encode(row.try_get::<Vec<u8>, _>("event_id")?),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    );

    let mut activity = vec![ActivityEntry {
        event_id: hex::encode(row.try_get::<Vec<u8>, _>("created_event_id")?),
        actor_pubkey: hex::encode(row.try_get::<Vec<u8>, _>("created_by")?),
        accepted_at: row.try_get("accepted_at")?,
        client_created_at: None,
        action: "instance.insert".into(),
        item_id: None,
        structure_revision: Some(1),
    }];
    let activity_rows = sqlx::query(
        "SELECT event_id, actor, accepted_at, client_created_at, action, item_id, \
                NULL::BIGINT AS structure_revision \
         FROM playbook_item_actions WHERE community_id = $1 AND instance_id = $2 \
         UNION ALL \
         SELECT event_id, actor, accepted_at, NULL::TIMESTAMPTZ AS client_created_at, \
                operation_type AS action, NULL::UUID AS item_id, \
                result_revision AS structure_revision \
         FROM playbook_structure_operations WHERE community_id = $1 AND instance_id = $2 \
         ORDER BY accepted_at ASC, event_id ASC",
    )
    .bind(community_id.as_uuid())
    .bind(instance_id)
    .fetch_all(pool)
    .await?;
    for entry in activity_rows {
        activity.push(ActivityEntry {
            event_id: hex::encode(entry.try_get::<Vec<u8>, _>("event_id")?),
            actor_pubkey: hex::encode(entry.try_get::<Vec<u8>, _>("actor")?),
            accepted_at: entry.try_get("accepted_at")?,
            client_created_at: entry.try_get("client_created_at")?,
            action: entry.try_get("action")?,
            item_id: entry.try_get("item_id")?,
            structure_revision: entry
                .try_get::<Option<i64>, _>("structure_revision")?
                .map(|v| v as u64),
        });
    }
    activity.sort_by(|left, right| {
        left.accepted_at
            .cmp(&right.accepted_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    Ok(InstanceSnapshot {
        instance,
        item_states,
        activity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_core::playbook::{
        PlaybookItem, PlaybookSection, StructureOperationKind, PLAYBOOK_SCHEMA_VERSION,
    };

    async fn setup_fixture() -> (PgPool, CommunityId, Uuid, [u8; 32], [u8; 32]) {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .expect("BUZZ_TEST_DATABASE_URL or DATABASE_URL");
        let pool = PgPool::connect(&database_url)
            .await
            .expect("connect test DB");
        let community_id = CommunityId::from_uuid(Uuid::new_v4());
        let channel_id = Uuid::new_v4();
        let owner = [1_u8; 32];
        let member = [2_u8; 32];
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(community_id.as_uuid())
            .bind(format!("playbook-{}.test", community_id.as_uuid()))
            .execute(&pool)
            .await
            .expect("insert community");
        sqlx::query(
            "INSERT INTO relay_members (community_id, pubkey, role) \
             VALUES ($1, $2, 'owner')",
        )
        .bind(community_id.as_uuid())
        .bind(hex::encode(owner))
        .execute(&pool)
        .await
        .expect("insert workspace owner");
        sqlx::query(
            "INSERT INTO channels (community_id, id, name, created_by) \
             VALUES ($1, $2, 'Playbook test', $3)",
        )
        .bind(community_id.as_uuid())
        .bind(channel_id)
        .bind(owner.as_slice())
        .execute(&pool)
        .await
        .expect("insert channel");
        sqlx::query(
            "INSERT INTO channel_members (community_id, channel_id, pubkey, role) \
             VALUES ($1, $2, $3, 'owner'), ($1, $2, $4, 'member')",
        )
        .bind(community_id.as_uuid())
        .bind(channel_id)
        .bind(owner.as_slice())
        .bind(member.as_slice())
        .execute(&pool)
        .await
        .expect("insert channel members");
        (pool, community_id, channel_id, owner, member)
    }

    fn template(template_id: Uuid, item_id: Uuid, revision: u64) -> TemplateRevision {
        TemplateRevision {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            template_id,
            revision,
            name: format!("Launch v{revision}"),
            description: None,
            status: TemplateStatus::Active,
            sections: vec![PlaybookSection {
                section_id: Uuid::new_v4(),
                title: "Discovery".into(),
                position: 1000,
                deleted: false,
                items: vec![PlaybookItem {
                    item_id,
                    text: format!("Collect access v{revision}"),
                    position: 1000,
                    deleted: false,
                }],
            }],
        }
    }

    async fn cleanup(pool: &PgPool, community_id: CommunityId) {
        sqlx::query("DELETE FROM playbook_item_actions WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete action fixture rows");
        sqlx::query("DELETE FROM playbook_structure_operations WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete operation fixture rows");
        sqlx::query("DELETE FROM playbook_instances WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete instance fixture rows");
        sqlx::query("DELETE FROM playbook_template_revisions WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete revision fixture rows");
        sqlx::query("DELETE FROM playbook_templates WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete template fixture rows");
        sqlx::query("DELETE FROM channels WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete channel fixture");
        sqlx::query("DELETE FROM relay_members WHERE community_id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete relay member fixture");
        sqlx::query("DELETE FROM communities WHERE id = $1")
            .bind(community_id.as_uuid())
            .execute(pool)
            .await
            .expect("delete community fixture");
    }

    #[tokio::test]
    #[ignore = "requires migrated Postgres"]
    async fn phase_one_projection_contract() {
        let (pool, community_id, channel_id, owner, member) = setup_fixture().await;
        let template_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let first = template(template_id, item_id, 1);
        apply_template_revision(&pool, community_id, &first, &[11; 32], &owner)
            .await
            .expect("create template");

        let request = InstanceInsert {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            instance_id,
            channel_id,
            template_id,
        };
        let inserted = insert_instance(&pool, community_id, &request, &[12; 32], &owner)
            .await
            .expect("insert instance");
        assert_eq!(inserted.instance.source_template_revision, 1);
        assert_eq!(
            inserted.instance.sections[0].items[0].text,
            "Collect access v1"
        );

        let second = template(template_id, item_id, 2);
        apply_template_revision(&pool, community_id, &second, &[13; 32], &owner)
            .await
            .expect("revise template");
        let unchanged = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("read existing instance");
        assert_eq!(
            unchanged.instance.sections[0].items[0].text,
            "Collect access v1"
        );

        let client_created_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let action = ItemAction {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            action_id: Uuid::new_v4(),
            instance_id,
            item_id,
            action: ItemActionKind::Complete,
            client_created_at,
        };
        let completed =
            apply_item_action(&pool, community_id, channel_id, &action, &[14; 32], &member)
                .await
                .expect("member completes item");
        assert!(completed.applied);
        assert_eq!(completed.canonical_event_id, vec![14; 32]);
        assert!(completed.snapshot.item_states[0].completed);
        assert_eq!(
            completed.snapshot.item_states[0].client_created_at,
            Some(client_created_at)
        );
        let replay =
            apply_item_action(&pool, community_id, channel_id, &action, &[19; 32], &member)
                .await
                .expect("semantic action retry in a new wrapper");
        assert!(!replay.applied);
        assert_eq!(replay.canonical_event_id, vec![14; 32]);
        assert_eq!(replay.snapshot.activity.len(), 2);

        assert!(matches!(
            apply_item_action(&pool, community_id, channel_id, &action, &[20; 32], &owner).await,
            Err(DbError::Conflict(_))
        ));
        let changed_action = ItemAction {
            action: ItemActionKind::Reopen,
            ..action.clone()
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &changed_action,
                &[21; 32],
                &member,
            )
            .await,
            Err(DbError::Conflict(_))
        ));

        let member_edit = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id,
            base_structure_revision: 1,
            operation: StructureOperationKind::InstanceRename {
                name: "Member edit".into(),
            },
        };
        assert!(matches!(
            apply_structure_operation(
                &pool,
                community_id,
                channel_id,
                &member_edit,
                &[15; 32],
                &member,
            )
            .await,
            Err(DbError::AccessDenied(_))
        ));

        let owner_edit = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id,
            base_structure_revision: 1,
            operation: StructureOperationKind::InstanceRename {
                name: "Channel copy".into(),
            },
        };
        let edited = apply_structure_operation(
            &pool,
            community_id,
            channel_id,
            &owner_edit,
            &[16; 32],
            &owner,
        )
        .await
        .expect("owner edits instance");
        assert!(edited.applied);
        assert_eq!(edited.snapshot.instance.structure_revision, 2);
        let edit_replay = apply_structure_operation(
            &pool,
            community_id,
            channel_id,
            &owner_edit,
            &[22; 32],
            &owner,
        )
        .await
        .expect("semantic structure retry in a new wrapper");
        assert!(!edit_replay.applied);
        assert_eq!(edit_replay.canonical_event_id, vec![16; 32]);
        assert_eq!(edit_replay.snapshot.instance.structure_revision, 2);
        assert_eq!(edit_replay.snapshot.activity.len(), 3);

        let changed_edit = StructureOperation {
            operation: StructureOperationKind::InstanceRename {
                name: "Changed reuse".into(),
            },
            ..owner_edit.clone()
        };
        assert!(matches!(
            apply_structure_operation(
                &pool,
                community_id,
                channel_id,
                &changed_edit,
                &[23; 32],
                &owner,
            )
            .await,
            Err(DbError::Conflict(_))
        ));

        let stale = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id,
            base_structure_revision: 1,
            operation: StructureOperationKind::InstanceRename {
                name: "Stale".into(),
            },
        };
        assert!(matches!(
            apply_structure_operation(&pool, community_id, channel_id, &stale, &[17; 32], &owner,)
                .await,
            Err(DbError::Conflict(_))
        ));
        let after_conflict = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("read after conflict");
        assert_eq!(after_conflict.instance.name, "Channel copy");
        assert_eq!(after_conflict.instance.structure_revision, 2);

        sqlx::query(
            "UPDATE channel_members SET removed_at = NOW() \
             WHERE community_id = $1 AND channel_id = $2 AND pubkey = $3",
        )
        .bind(community_id.as_uuid())
        .bind(channel_id)
        .bind(member.as_slice())
        .execute(&pool)
        .await
        .expect("remove member");
        let removed_action = ItemAction {
            action_id: Uuid::new_v4(),
            ..action
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &removed_action,
                &[18; 32],
                &member,
            )
            .await,
            Err(DbError::AccessDenied(_))
        ));

        cleanup(&pool, community_id).await;
    }

    #[tokio::test]
    #[ignore = "requires migrated Postgres"]
    async fn semantic_idempotency_and_concurrency_contract() {
        let (pool, community_id, channel_id, owner, member_a) = setup_fixture().await;
        let member_b = [3_u8; 32];
        sqlx::query(
            "INSERT INTO channel_members (community_id, channel_id, pubkey, role) \
             VALUES ($1, $2, $3, 'member')",
        )
        .bind(community_id.as_uuid())
        .bind(channel_id)
        .bind(member_b.as_slice())
        .execute(&pool)
        .await
        .expect("insert second member");

        let template_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        apply_template_revision(
            &pool,
            community_id,
            &template(template_id, item_id, 1),
            &[30; 32],
            &owner,
        )
        .await
        .expect("create template");
        insert_instance(
            &pool,
            community_id,
            &InstanceInsert {
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                instance_id,
                channel_id,
                template_id,
            },
            &[31; 32],
            &owner,
        )
        .await
        .expect("insert instance");

        let action = ItemAction {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            action_id: Uuid::new_v4(),
            instance_id,
            item_id,
            action: ItemActionKind::Complete,
            client_created_at: DateTime::from_timestamp(1_700_000_000, 123_456_000).unwrap(),
        };
        let (left, right) = tokio::join!(
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &action,
                &[32; 32],
                &member_a,
            ),
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &action,
                &[33; 32],
                &member_a,
            ),
        );
        let left = left.expect("first semantic action wrapper");
        let right = right.expect("second semantic action wrapper");
        assert_ne!(left.applied, right.applied);
        assert_eq!(left.canonical_event_id, right.canonical_event_id);
        assert_eq!(left.snapshot.activity.len(), 2);
        assert_eq!(right.snapshot.activity.len(), 2);

        let operation = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id,
            base_structure_revision: 1,
            operation: StructureOperationKind::InstanceRename {
                name: "Semantic rename".into(),
            },
        };
        let (left, right) = tokio::join!(
            apply_structure_operation(
                &pool,
                community_id,
                channel_id,
                &operation,
                &[34; 32],
                &owner,
            ),
            apply_structure_operation(
                &pool,
                community_id,
                channel_id,
                &operation,
                &[35; 32],
                &owner,
            ),
        );
        let left = left.expect("first semantic operation wrapper");
        let right = right.expect("second semantic operation wrapper");
        assert_ne!(left.applied, right.applied);
        assert_eq!(left.canonical_event_id, right.canonical_event_id);
        assert_eq!(left.snapshot.instance.structure_revision, 2);
        assert_eq!(right.snapshot.instance.structure_revision, 2);
        assert_eq!(left.snapshot.activity.len(), 3);
        assert_eq!(right.snapshot.activity.len(), 3);

        let race_a = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id,
            base_structure_revision: 2,
            operation: StructureOperationKind::InstanceRename {
                name: "Race A".into(),
            },
        };
        let race_b = StructureOperation {
            operation_id: Uuid::new_v4(),
            operation: StructureOperationKind::InstanceRename {
                name: "Race B".into(),
            },
            ..race_a.clone()
        };
        let (race_a_result, race_b_result) = tokio::join!(
            apply_structure_operation(&pool, community_id, channel_id, &race_a, &[36; 32], &owner,),
            apply_structure_operation(&pool, community_id, channel_id, &race_b, &[37; 32], &owner,),
        );
        assert_eq!(
            usize::from(race_a_result.is_ok()) + usize::from(race_b_result.is_ok()),
            1
        );
        assert!(matches!(
            race_a_result
                .as_ref()
                .err()
                .or(race_b_result.as_ref().err()),
            Some(DbError::Conflict(_))
        ));
        let after_race = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("read after structural race");
        assert_eq!(after_race.instance.structure_revision, 3);
        assert!(matches!(
            after_race.instance.name.as_str(),
            "Race A" | "Race B"
        ));

        let complete = ItemAction {
            action_id: Uuid::new_v4(),
            client_created_at: DateTime::from_timestamp(1_700_000_001, 0).unwrap(),
            ..action.clone()
        };
        let reopen = ItemAction {
            action_id: Uuid::new_v4(),
            action: ItemActionKind::Reopen,
            client_created_at: DateTime::from_timestamp(1_700_000_002, 0).unwrap(),
            ..action
        };
        let (complete_result, reopen_result) = tokio::join!(
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &complete,
                &[38; 32],
                &member_a,
            ),
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &reopen,
                &[39; 32],
                &member_b,
            ),
        );
        complete_result.expect("member A concurrent action");
        reopen_result.expect("member B concurrent action");
        let concurrent = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("read concurrent item actions");
        assert_eq!(
            concurrent
                .activity
                .iter()
                .filter(|entry| matches!(entry.action.as_str(), "complete" | "reopen"))
                .count(),
            3
        );
        assert!(concurrent
            .activity
            .iter()
            .any(|entry| entry.actor_pubkey == hex::encode(member_a)));
        assert!(concurrent
            .activity
            .iter()
            .any(|entry| entry.actor_pubkey == hex::encode(member_b)));

        let tied_at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        sqlx::query(
            "UPDATE playbook_item_actions SET accepted_at = $1 \
             WHERE community_id = $2 AND action_id IN ($3, $4)",
        )
        .bind(tied_at)
        .bind(community_id.as_uuid())
        .bind(complete.action_id)
        .bind(reopen.action_id)
        .execute(&pool)
        .await
        .expect("force equal durable acceptance time");
        let rebuilt = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("rebuild from durable action rows");
        let visible = rebuilt
            .item_states
            .iter()
            .find(|state| state.item_id == item_id)
            .expect("visible item state");
        assert!(
            !visible.completed,
            "larger event ID must win equal-time tie"
        );
        assert_eq!(visible.event_id, hex::encode([39; 32]));
        assert_eq!(
            instance_snapshot(&pool, community_id, instance_id)
                .await
                .expect("repeat deterministic rebuild"),
            rebuilt
        );

        cleanup(&pool, community_id).await;
    }

    #[tokio::test]
    #[ignore = "requires migrated Postgres"]
    async fn every_structural_operation_is_atomic_at_the_database_boundary() {
        let (pool, community_id, channel_id, owner, _) = setup_fixture().await;
        let template_id = Uuid::new_v4();
        let original_item_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let source = template(template_id, original_item_id, 1);
        let original_section_id = source.sections[0].section_id;
        apply_template_revision(&pool, community_id, &source, &[50; 32], &owner)
            .await
            .expect("create template");
        insert_instance(
            &pool,
            community_id,
            &InstanceInsert {
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                instance_id,
                channel_id,
                template_id,
            },
            &[51; 32],
            &owner,
        )
        .await
        .expect("insert instance");

        let added_section_id = Uuid::new_v4();
        let added_item_id = Uuid::new_v4();
        let duplicate_item_id = Uuid::new_v4();
        let operations = vec![
            StructureOperationKind::SectionAdd {
                section: PlaybookSection {
                    section_id: added_section_id,
                    title: "Launch".into(),
                    position: 2000,
                    deleted: false,
                    items: vec![],
                },
            },
            StructureOperationKind::SectionUpdate {
                section_id: added_section_id,
                title: "Launch day".into(),
            },
            StructureOperationKind::SectionReorder {
                section_id: added_section_id,
                position: 500,
            },
            StructureOperationKind::SectionRemove {
                section_id: added_section_id,
            },
            StructureOperationKind::SectionRestore {
                section_id: added_section_id,
            },
            StructureOperationKind::ItemAdd {
                section_id: original_section_id,
                item: PlaybookItem {
                    item_id: added_item_id,
                    text: "Confirm owner".into(),
                    position: 2000,
                    deleted: false,
                },
            },
            StructureOperationKind::ItemUpdate {
                item_id: added_item_id,
                text: "Confirm launch owner".into(),
            },
            StructureOperationKind::ItemReorder {
                item_id: added_item_id,
                position: 500,
            },
            StructureOperationKind::ItemRemove {
                item_id: added_item_id,
            },
            StructureOperationKind::ItemRestore {
                item_id: added_item_id,
            },
            StructureOperationKind::ItemDuplicate {
                item_id: original_item_id,
                new_item_id: duplicate_item_id,
                position: 3000,
            },
            StructureOperationKind::InstanceRename {
                name: "Channel launch".into(),
            },
            StructureOperationKind::InstanceArchive,
        ];

        for (index, operation) in operations.iter().cloned().enumerate() {
            let result = apply_structure_operation(
                &pool,
                community_id,
                channel_id,
                &StructureOperation {
                    schema_version: PLAYBOOK_SCHEMA_VERSION,
                    operation_id: Uuid::new_v4(),
                    instance_id,
                    base_structure_revision: index as u64 + 1,
                    operation,
                },
                &[index as u8 + 52; 32],
                &owner,
            )
            .await
            .unwrap_or_else(|error| panic!("operation {} failed: {error}", index + 1));
            assert!(result.applied);
            assert_eq!(
                result.snapshot.instance.structure_revision,
                index as u64 + 2
            );
        }

        let snapshot = instance_snapshot(&pool, community_id, instance_id)
            .await
            .expect("read final structural projection");
        assert_eq!(snapshot.instance.status, InstanceStatus::Archived);
        assert_eq!(snapshot.instance.structure_revision, 14);
        assert_eq!(snapshot.instance.name, "Channel launch");
        assert_eq!(snapshot.activity.len(), 14);
        assert_eq!(
            snapshot
                .activity
                .iter()
                .skip(1)
                .map(|entry| entry.action.as_str())
                .collect::<Vec<_>>(),
            vec![
                "section.add",
                "section.update",
                "section.reorder",
                "section.remove",
                "section.restore",
                "item.add",
                "item.update",
                "item.reorder",
                "item.remove",
                "item.restore",
                "item.duplicate",
                "instance.rename",
                "instance.archive",
            ]
        );
        let added_section = snapshot
            .instance
            .sections
            .iter()
            .find(|section| section.section_id == added_section_id)
            .expect("stable restored section ID");
        assert_eq!(added_section.title, "Launch day");
        assert_eq!(added_section.position, 500);
        assert!(!added_section.deleted);
        let added_item = snapshot
            .instance
            .sections
            .iter()
            .flat_map(|section| &section.items)
            .find(|item| item.item_id == added_item_id)
            .expect("stable restored item ID");
        assert_eq!(added_item.text, "Confirm launch owner");
        assert_eq!(added_item.position, 500);
        assert!(!added_item.deleted);
        assert!(snapshot
            .instance
            .sections
            .iter()
            .flat_map(|section| &section.items)
            .any(|item| item.item_id == duplicate_item_id));

        let archived_action = ItemAction {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            action_id: Uuid::new_v4(),
            instance_id,
            item_id: original_item_id,
            action: ItemActionKind::Complete,
            client_created_at: Utc::now(),
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_id,
                &archived_action,
                &[70; 32],
                &owner,
            )
            .await,
            Err(DbError::NotFound(_))
        ));

        cleanup(&pool, community_id).await;
    }

    #[tokio::test]
    #[ignore = "requires migrated Postgres"]
    async fn role_table_cross_channel_rejection_and_isolation_contract() {
        let (pool, community_id, channel_a, owner, member) = setup_fixture().await;
        let channel_b = Uuid::new_v4();
        let admin = [4_u8; 32];
        let non_member = [5_u8; 32];
        sqlx::query(
            "INSERT INTO relay_members (community_id, pubkey, role) VALUES \
             ($1, $2, 'admin'), ($1, $3, 'member')",
        )
        .bind(community_id.as_uuid())
        .bind(hex::encode(admin))
        .bind(hex::encode(member))
        .execute(&pool)
        .await
        .expect("insert workspace roles");
        sqlx::query(
            "INSERT INTO channels (community_id, id, name, created_by) \
             VALUES ($1, $2, 'Playbook test B', $3)",
        )
        .bind(community_id.as_uuid())
        .bind(channel_b)
        .bind(owner.as_slice())
        .execute(&pool)
        .await
        .expect("insert second channel");
        sqlx::query(
            "INSERT INTO channel_members (community_id, channel_id, pubkey, role) VALUES \
             ($1, $2, $3, 'admin'), ($1, $4, $3, 'admin'), ($1, $4, $5, 'member')",
        )
        .bind(community_id.as_uuid())
        .bind(channel_a)
        .bind(admin.as_slice())
        .bind(channel_b)
        .bind(member.as_slice())
        .execute(&pool)
        .await
        .expect("insert channel role fixtures");

        let template_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let source = template(template_id, item_id, 1);
        apply_template_revision(&pool, community_id, &source, &[80; 32], &admin)
            .await
            .expect("workspace admin creates template");
        let denied_template = template(Uuid::new_v4(), Uuid::new_v4(), 1);
        assert!(matches!(
            apply_template_revision(&pool, community_id, &denied_template, &[81; 32], &member,)
                .await,
            Err(DbError::AccessDenied(_))
        ));

        let instance_a = Uuid::new_v4();
        let instance_b = Uuid::new_v4();
        let inserted_a = insert_instance(
            &pool,
            community_id,
            &InstanceInsert {
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                instance_id: instance_a,
                channel_id: channel_a,
                template_id,
            },
            &[82; 32],
            &admin,
        )
        .await
        .expect("channel admin inserts A");
        let inserted_b = insert_instance(
            &pool,
            community_id,
            &InstanceInsert {
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                instance_id: instance_b,
                channel_id: channel_b,
                template_id,
            },
            &[83; 32],
            &admin,
        )
        .await
        .expect("channel admin inserts B");
        assert_ne!(instance_a, instance_b);
        assert_eq!(inserted_a.instance.sections, inserted_b.instance.sections);
        assert!(matches!(
            insert_instance(
                &pool,
                community_id,
                &InstanceInsert {
                    schema_version: PLAYBOOK_SCHEMA_VERSION,
                    instance_id: Uuid::new_v4(),
                    channel_id: channel_a,
                    template_id,
                },
                &[84; 32],
                &admin,
            )
            .await,
            Err(DbError::Conflict(_))
        ));

        let source_bytes = serde_json::to_vec(
            &template_snapshot(&pool, community_id, template_id)
                .await
                .expect("template snapshot")
                .template,
        )
        .unwrap();
        let instance_b_bytes = serde_json::to_vec(&inserted_b.instance).unwrap();
        let edit_a = StructureOperation {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            instance_id: instance_a,
            base_structure_revision: 1,
            operation: StructureOperationKind::InstanceRename {
                name: "Only channel A".into(),
            },
        };
        apply_structure_operation(&pool, community_id, channel_a, &edit_a, &[85; 32], &admin)
            .await
            .expect("channel admin edits A");
        assert_eq!(
            serde_json::to_vec(
                &template_snapshot(&pool, community_id, template_id)
                    .await
                    .expect("template unchanged")
                    .template
            )
            .unwrap(),
            source_bytes
        );
        assert_eq!(
            serde_json::to_vec(
                &instance_snapshot(&pool, community_id, instance_b)
                    .await
                    .expect("B unchanged")
                    .instance
            )
            .unwrap(),
            instance_b_bytes
        );

        let member_action = ItemAction {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            action_id: Uuid::new_v4(),
            instance_id: instance_b,
            item_id,
            action: ItemActionKind::Complete,
            client_created_at: Utc::now(),
        };
        apply_item_action(
            &pool,
            community_id,
            channel_b,
            &member_action,
            &[86; 32],
            &member,
        )
        .await
        .expect("current member completes item");
        assert!(matches!(
            apply_structure_operation(
                &pool,
                community_id,
                channel_b,
                &StructureOperation {
                    operation_id: Uuid::new_v4(),
                    instance_id: instance_b,
                    base_structure_revision: 1,
                    operation: StructureOperationKind::InstanceRename {
                        name: "Denied member edit".into(),
                    },
                    schema_version: PLAYBOOK_SCHEMA_VERSION,
                },
                &[87; 32],
                &member,
            )
            .await,
            Err(DbError::AccessDenied(_))
        ));

        let wrong_channel_action = ItemAction {
            action_id: Uuid::new_v4(),
            instance_id: instance_a,
            ..member_action.clone()
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_b,
                &wrong_channel_action,
                &[88; 32],
                &member,
            )
            .await,
            Err(DbError::NotFound(_))
        ));
        let outsider_action = ItemAction {
            action_id: Uuid::new_v4(),
            ..member_action.clone()
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_b,
                &outsider_action,
                &[89; 32],
                &non_member,
            )
            .await,
            Err(DbError::AccessDenied(_))
        ));
        sqlx::query(
            "UPDATE channel_members SET removed_at = NOW() \
             WHERE community_id = $1 AND channel_id = $2 AND pubkey = $3",
        )
        .bind(community_id.as_uuid())
        .bind(channel_b)
        .bind(member.as_slice())
        .execute(&pool)
        .await
        .expect("remove member");
        let removed_action = ItemAction {
            action_id: Uuid::new_v4(),
            ..member_action
        };
        assert!(matches!(
            apply_item_action(
                &pool,
                community_id,
                channel_b,
                &removed_action,
                &[90; 32],
                &member,
            )
            .await,
            Err(DbError::AccessDenied(_))
        ));

        cleanup(&pool, community_id).await;
    }
}
