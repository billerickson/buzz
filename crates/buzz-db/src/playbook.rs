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
) -> Result<InstanceSnapshot> {
    if action.schema_version != buzz_core::playbook::PLAYBOOK_SCHEMA_VERSION {
        return Err(DbError::InvalidData(format!(
            "unsupported schema_version {}",
            action.schema_version
        )));
    }
    let mut tx = pool.begin().await?;
    require_channel_role(&mut tx, community_id, channel_id, actor, false).await?;

    if let Some(row) = sqlx::query(
        "SELECT instance_id, item_id, action, event_id FROM playbook_item_actions \
         WHERE community_id = $1 AND action_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(action.action_id)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let same = row.try_get::<Uuid, _>("instance_id")? == action.instance_id
            && row.try_get::<Uuid, _>("item_id")? == action.item_id
            && row.try_get::<String, _>("action")? == item_action_name(action.action)
            && row.try_get::<Vec<u8>, _>("event_id")? == event_id;
        if !same {
            return Err(DbError::Conflict(
                "action_id was already used for a different action".into(),
            ));
        }
        tx.commit().await?;
        return instance_snapshot(pool, community_id, action.instance_id).await;
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
         (community_id, action_id, instance_id, item_id, action, event_id, actor, client_created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(community_id.as_uuid())
    .bind(action.action_id)
    .bind(action.instance_id)
    .bind(action.item_id)
    .bind(item_action_name(action.action))
    .bind(event_id)
    .bind(actor)
    .bind(action.client_created_at)
    .execute(tx.as_mut())
    .await?;
    tx.commit().await?;
    instance_snapshot(pool, community_id, action.instance_id).await
}

/// Apply an idempotent structural operation under a row lock.
pub async fn apply_structure_operation(
    pool: &PgPool,
    community_id: CommunityId,
    channel_id: Uuid,
    operation: &StructureOperation,
    event_id: &[u8],
    actor: &[u8],
) -> Result<InstanceSnapshot> {
    if operation.schema_version != buzz_core::playbook::PLAYBOOK_SCHEMA_VERSION {
        return Err(DbError::InvalidData(format!(
            "unsupported schema_version {}",
            operation.schema_version
        )));
    }
    let mut tx = pool.begin().await?;
    require_channel_role(&mut tx, community_id, channel_id, actor, true).await?;

    if let Some(row) = sqlx::query(
        "SELECT instance_id, event_id FROM playbook_structure_operations \
         WHERE community_id = $1 AND operation_id = $2",
    )
    .bind(community_id.as_uuid())
    .bind(operation.operation_id)
    .fetch_optional(tx.as_mut())
    .await?
    {
        let same = row.try_get::<Uuid, _>("instance_id")? == operation.instance_id
            && row.try_get::<Vec<u8>, _>("event_id")? == event_id;
        if !same {
            return Err(DbError::Conflict(
                "operation_id was already used for a different operation".into(),
            ));
        }
        tx.commit().await?;
        return instance_snapshot(pool, community_id, operation.instance_id).await;
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
          operation_type, event_id, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(community_id.as_uuid())
    .bind(operation.operation_id)
    .bind(operation.instance_id)
    .bind(operation.base_structure_revision as i64)
    .bind(next_revision)
    .bind(operation.operation.name())
    .bind(event_id)
    .bind(actor)
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
    instance_snapshot(pool, community_id, operation.instance_id).await
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
        assert!(completed.item_states[0].completed);
        assert_eq!(
            completed.item_states[0].client_created_at,
            Some(client_created_at)
        );
        let replay =
            apply_item_action(&pool, community_id, channel_id, &action, &[14; 32], &member)
                .await
                .expect("exact action retry");
        assert_eq!(replay.activity.len(), 2);

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
        assert_eq!(edited.instance.structure_revision, 2);

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
}
