//! `buzz playbooks` — structured native Playbooks commands.

use chrono::Utc;
use nostr::{EventBuilder, Kind, Tag};
use uuid::Uuid;

use buzz_core::kind::{
    KIND_PLAYBOOK_INSTANCE_INSERT, KIND_PLAYBOOK_INSTANCE_SNAPSHOT, KIND_PLAYBOOK_ITEM_ACTION,
    KIND_PLAYBOOK_STRUCTURE_OPERATION, KIND_PLAYBOOK_TEMPLATE_REVISION,
    KIND_PLAYBOOK_TEMPLATE_SNAPSHOT,
};
use buzz_core::playbook::{
    InstanceInsert, InstanceSnapshot, ItemAction, ItemActionKind, StructureOperation,
    TemplateRevision, PLAYBOOK_SCHEMA_VERSION,
};

use crate::client::create_response_with_id;
use crate::client::BuzzClient;
use crate::error::CliError;
use crate::{PlaybookTemplatesCmd, PlaybooksCmd};

/// Dispatch a Playbooks CLI command.
pub async fn dispatch(command: PlaybooksCmd, client: &BuzzClient) -> Result<(), CliError> {
    match command {
        PlaybooksCmd::Templates(command) => dispatch_templates(command, client).await,
        PlaybooksCmd::Insert { channel, template } => {
            let request = InstanceInsert {
                schema_version: PLAYBOOK_SCHEMA_VERSION,
                instance_id: Uuid::new_v4(),
                channel_id: channel,
                template_id: template,
            };
            let response = submit_response(
                client,
                KIND_PLAYBOOK_INSTANCE_INSERT,
                &request,
                vec![
                    exact_tag("h", channel)?,
                    exact_tag("instance", request.instance_id)?,
                    exact_tag("template", template)?,
                ],
            )
            .await?;
            println!(
                "{}",
                create_response_with_id(&response, "instance_id", &request.instance_id.to_string())
            );
            Ok(())
        }
        PlaybooksCmd::Get { instance } => {
            let snapshot = get_instance(client, instance).await?;
            print_json(&snapshot)
        }
        PlaybooksCmd::Check {
            instance,
            item,
            action_id,
            client_created_at,
        } => {
            submit_item_action(
                client,
                instance,
                item,
                ItemActionKind::Complete,
                action_id,
                client_created_at,
            )
            .await
        }
        PlaybooksCmd::Reopen {
            instance,
            item,
            action_id,
            client_created_at,
        } => {
            submit_item_action(
                client,
                instance,
                item,
                ItemActionKind::Reopen,
                action_id,
                client_created_at,
            )
            .await
        }
        PlaybooksCmd::Edit {
            instance,
            operation_file,
        } => {
            let snapshot = get_instance(client, instance).await?;
            let operation: StructureOperation = read_json(&operation_file)?;
            if operation.instance_id != instance {
                return Err(CliError::Usage(
                    "--instance does not match operation_file instance_id".into(),
                ));
            }
            submit(
                client,
                KIND_PLAYBOOK_STRUCTURE_OPERATION,
                &operation,
                vec![
                    exact_tag("h", snapshot.instance.channel_id)?,
                    exact_tag("instance", instance)?,
                ],
            )
            .await
        }
        PlaybooksCmd::Activity { instance } => {
            let snapshot = get_instance(client, instance).await?;
            print_json(&snapshot.activity)
        }
    }
}

async fn dispatch_templates(
    command: PlaybookTemplatesCmd,
    client: &BuzzClient,
) -> Result<(), CliError> {
    match command {
        PlaybookTemplatesCmd::List => {
            let events = client
                .query_all(serde_json::json!({
                    "kinds": [KIND_PLAYBOOK_TEMPLATE_SNAPSHOT],
                }))
                .await?;
            let snapshots = events
                .iter()
                .map(parse_content)
                .collect::<Result<Vec<serde_json::Value>, CliError>>()?;
            print_json(&snapshots)
        }
        PlaybookTemplatesCmd::Get { template } => {
            let event = query_snapshot(
                client,
                KIND_PLAYBOOK_TEMPLATE_SNAPSHOT,
                template,
                "template",
            )
            .await?;
            print_json(&parse_content::<serde_json::Value>(&event)?)
        }
        PlaybookTemplatesCmd::Create { file } => {
            let revision: TemplateRevision = read_json(&file)?;
            if revision.revision != 1 {
                return Err(CliError::Usage(
                    "template create requires revision 1".into(),
                ));
            }
            let response = submit_response(
                client,
                KIND_PLAYBOOK_TEMPLATE_REVISION,
                &revision,
                vec![exact_tag("template", revision.template_id)?],
            )
            .await?;
            println!(
                "{}",
                create_response_with_id(
                    &response,
                    "template_id",
                    &revision.template_id.to_string()
                )
            );
            Ok(())
        }
        PlaybookTemplatesCmd::Update { template, file } => {
            let revision: TemplateRevision = read_json(&file)?;
            if revision.template_id != template {
                return Err(CliError::Usage(
                    "--template does not match file template_id".into(),
                ));
            }
            submit(
                client,
                KIND_PLAYBOOK_TEMPLATE_REVISION,
                &revision,
                vec![exact_tag("template", template)?],
            )
            .await
        }
    }
}

async fn submit_item_action(
    client: &BuzzClient,
    instance: Uuid,
    item: Uuid,
    action: ItemActionKind,
    action_id: Option<Uuid>,
    client_created_at: Option<chrono::DateTime<Utc>>,
) -> Result<(), CliError> {
    let snapshot = get_instance(client, instance).await?;
    let payload = ItemAction {
        schema_version: PLAYBOOK_SCHEMA_VERSION,
        action_id: action_id.unwrap_or_else(Uuid::new_v4),
        instance_id: instance,
        item_id: item,
        action,
        client_created_at: client_created_at.unwrap_or_else(Utc::now),
    };
    submit(
        client,
        KIND_PLAYBOOK_ITEM_ACTION,
        &payload,
        vec![
            exact_tag("h", snapshot.instance.channel_id)?,
            exact_tag("instance", instance)?,
        ],
    )
    .await
}

async fn get_instance(client: &BuzzClient, instance: Uuid) -> Result<InstanceSnapshot, CliError> {
    let event = query_snapshot(
        client,
        KIND_PLAYBOOK_INSTANCE_SNAPSHOT,
        instance,
        "instance",
    )
    .await?;
    parse_content(&event)
}

async fn query_snapshot(
    client: &BuzzClient,
    kind: u32,
    id: Uuid,
    label: &str,
) -> Result<serde_json::Value, CliError> {
    let events = client
        .query_paginated(
            serde_json::json!({
                "kinds": [kind],
                "#d": [id.to_string()],
            }),
            2,
        )
        .await?;
    match events.as_slice() {
        [event] => Ok(event.clone()),
        [] => Err(CliError::NotFound(format!("{label} {id}"))),
        _ => Err(CliError::Other(format!(
            "relay returned multiple current snapshots for {label} {id}"
        ))),
    }
}

fn parse_content<T: serde::de::DeserializeOwned>(event: &serde_json::Value) -> Result<T, CliError> {
    let content = event
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CliError::Other("snapshot event has no content".into()))?;
    serde_json::from_str(content)
        .map_err(|error| CliError::Other(format!("invalid snapshot JSON: {error}")))
}

fn exact_tag(name: &str, value: impl ToString) -> Result<Tag, CliError> {
    Tag::parse([name, value.to_string().as_str()])
        .map_err(|error| CliError::Other(format!("{name} tag error: {error}")))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, CliError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| CliError::Other(format!("failed to read {path}: {error}")))?;
    serde_json::from_str(&content)
        .map_err(|error| CliError::Usage(format!("invalid JSON in {path}: {error}")))
}

async fn submit<T: serde::Serialize>(
    client: &BuzzClient,
    kind: u32,
    payload: &T,
    tags: Vec<Tag>,
) -> Result<(), CliError> {
    let response = submit_response(client, kind, payload, tags).await?;
    println!("{response}");
    Ok(())
}

async fn submit_response<T: serde::Serialize>(
    client: &BuzzClient,
    kind: u32,
    payload: &T,
    tags: Vec<Tag>,
) -> Result<String, CliError> {
    let content = serde_json::to_string(payload)
        .map_err(|error| CliError::Other(format!("payload serialization failed: {error}")))?;
    let event =
        client.sign_event(EventBuilder::new(Kind::Custom(kind as u16), content).tags(tags))?;
    match client.submit_event(event).await {
        Ok(response) => Ok(response),
        Err(CliError::Relay { body, .. }) if body.starts_with("conflict:") => {
            Err(CliError::Conflict(conflict_detail(&body)))
        }
        Err(error) => Err(error),
    }
}

fn conflict_detail(body: &str) -> String {
    body.strip_prefix("conflict:")
        .unwrap_or(body)
        .trim_start()
        .to_owned()
}

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let output = serde_json::to_string(value)
        .map_err(|error| CliError::Other(format!("output serialization failed: {error}")))?;
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_file_round_trip_shape() {
        let json = serde_json::json!({
            "schema_version": 1,
            "template_id": Uuid::nil(),
            "revision": 1,
            "name": "Launch",
            "status": "active",
            "sections": []
        });
        let revision: TemplateRevision = serde_json::from_value(json).unwrap();
        assert_eq!(revision.revision, 1);
    }

    #[test]
    fn conflict_variant_formats_prefix_once() {
        let error = CliError::Conflict(conflict_detail("conflict: current revision 2"));
        assert_eq!(error.to_string(), "conflict: current revision 2");
    }
}
