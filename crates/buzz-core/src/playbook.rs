//! Native Buzz Playbook wire types and deterministic projection rules.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current playbook payload schema version.
pub const PLAYBOOK_SCHEMA_VERSION: u32 = 1;

/// Lifecycle state of a reusable template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateStatus {
    /// May be edited and inserted into channels.
    Active,
    /// Retained for history but unavailable for new insertions.
    Archived,
    /// Soft-deleted; allowed only while no instance references the template.
    Deleted,
}

/// Lifecycle state of a channel instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    /// Visible and mutable.
    Active,
    /// Read-only while its event and activity history remain retained.
    Archived,
}

/// Ordered checklist item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookItem {
    /// Stable item identifier.
    pub item_id: Uuid,
    /// User-visible checklist text.
    pub text: String,
    /// Sparse numeric ordering key.
    pub position: i64,
    /// Whether the item is hidden while retaining its identity and history.
    #[serde(default)]
    pub deleted: bool,
}

/// Ordered section containing checklist items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookSection {
    /// Stable section identifier.
    pub section_id: Uuid,
    /// User-visible section title.
    pub title: String,
    /// Sparse numeric ordering key.
    pub position: i64,
    /// Ordered checklist items.
    pub items: Vec<PlaybookItem>,
    /// Whether the section is hidden while retaining its contents and history.
    #[serde(default)]
    pub deleted: bool,
}

/// Immutable full template revision submitted by an owner or workspace admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRevision {
    /// Payload schema version.
    pub schema_version: u32,
    /// Stable template identifier.
    pub template_id: Uuid,
    /// Monotonically increasing revision, starting at one.
    pub revision: u64,
    /// User-visible template name.
    pub name: String,
    /// Optional template description.
    #[serde(default)]
    pub description: Option<String>,
    /// Template lifecycle state.
    pub status: TemplateStatus,
    /// Ordered template definition.
    pub sections: Vec<PlaybookSection>,
}

/// Signed request to deep-copy the latest active template into a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInsert {
    /// Payload schema version.
    pub schema_version: u32,
    /// Client-generated idempotent instance identifier.
    pub instance_id: Uuid,
    /// Target channel identifier; must match the event's `h` tag.
    pub channel_id: Uuid,
    /// Source template identifier.
    pub template_id: Uuid,
}

/// Materialized channel-specific playbook definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookInstance {
    /// Payload schema version.
    pub schema_version: u32,
    /// Stable instance identifier.
    pub instance_id: Uuid,
    /// Owning channel identifier.
    pub channel_id: Uuid,
    /// User-visible instance name.
    pub name: String,
    /// Source template identifier, retained for provenance only.
    pub source_template_id: Uuid,
    /// Source template revision, retained for provenance only.
    pub source_template_revision: u64,
    /// Monotonically increasing structural revision.
    pub structure_revision: u64,
    /// Instance lifecycle state.
    pub status: InstanceStatus,
    /// Deep-copied, instance-owned section and item data.
    pub sections: Vec<PlaybookSection>,
}

/// Checklist state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemActionKind {
    /// Mark an item complete.
    Complete,
    /// Reopen a completed item.
    Reopen,
}

/// Append-only signed item state transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemAction {
    /// Payload schema version.
    pub schema_version: u32,
    /// Client-generated idempotency identifier.
    pub action_id: Uuid,
    /// Target instance identifier.
    pub instance_id: Uuid,
    /// Target item identifier.
    pub item_id: Uuid,
    /// Requested state transition.
    pub action: ItemActionKind,
    /// Advisory client timestamp. Relay acceptance time is canonical.
    pub client_created_at: DateTime<Utc>,
}

/// A structural edit to an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum StructureOperationKind {
    /// Add a section.
    #[serde(rename = "section.add")]
    SectionAdd {
        /// New section value.
        section: PlaybookSection,
    },
    /// Rename a section.
    #[serde(rename = "section.update")]
    SectionUpdate {
        /// Target section.
        section_id: Uuid,
        /// Replacement title.
        title: String,
    },
    /// Tombstone a section.
    #[serde(rename = "section.remove")]
    SectionRemove {
        /// Target section.
        section_id: Uuid,
    },
    /// Restore a tombstoned section.
    #[serde(rename = "section.restore")]
    SectionRestore {
        /// Target section.
        section_id: Uuid,
    },
    /// Change a section ordering key.
    #[serde(rename = "section.reorder")]
    SectionReorder {
        /// Target section.
        section_id: Uuid,
        /// Replacement sparse position.
        position: i64,
    },
    /// Add an item to a section.
    #[serde(rename = "item.add")]
    ItemAdd {
        /// Parent section.
        section_id: Uuid,
        /// New item value.
        item: PlaybookItem,
    },
    /// Edit an item.
    #[serde(rename = "item.update")]
    ItemUpdate {
        /// Target item.
        item_id: Uuid,
        /// Replacement checklist text.
        text: String,
    },
    /// Tombstone an item.
    #[serde(rename = "item.remove")]
    ItemRemove {
        /// Target item.
        item_id: Uuid,
    },
    /// Restore a tombstoned item under its original section.
    #[serde(rename = "item.restore")]
    ItemRestore {
        /// Target item.
        item_id: Uuid,
    },
    /// Change an item ordering key.
    #[serde(rename = "item.reorder")]
    ItemReorder {
        /// Target item.
        item_id: Uuid,
        /// Replacement sparse position.
        position: i64,
    },
    /// Copy an active item under the same section with a fresh stable ID.
    #[serde(rename = "item.duplicate")]
    ItemDuplicate {
        /// Source item.
        item_id: Uuid,
        /// Fresh identifier for the copy.
        new_item_id: Uuid,
        /// Sparse position for the copy.
        position: i64,
    },
    /// Rename the channel-specific instance.
    #[serde(rename = "instance.rename")]
    InstanceRename {
        /// Replacement instance name.
        name: String,
    },
    /// Archive the instance while retaining activity history.
    #[serde(rename = "instance.archive")]
    InstanceArchive,
}

impl StructureOperationKind {
    /// Stable operation name used by audit projections and telemetry.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::SectionAdd { .. } => "section.add",
            Self::SectionUpdate { .. } => "section.update",
            Self::SectionRemove { .. } => "section.remove",
            Self::SectionRestore { .. } => "section.restore",
            Self::SectionReorder { .. } => "section.reorder",
            Self::ItemAdd { .. } => "item.add",
            Self::ItemUpdate { .. } => "item.update",
            Self::ItemRemove { .. } => "item.remove",
            Self::ItemRestore { .. } => "item.restore",
            Self::ItemReorder { .. } => "item.reorder",
            Self::ItemDuplicate { .. } => "item.duplicate",
            Self::InstanceRename { .. } => "instance.rename",
            Self::InstanceArchive => "instance.archive",
        }
    }
}

/// Signed optimistic-concurrency structural operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureOperation {
    /// Payload schema version.
    pub schema_version: u32,
    /// Client-generated idempotency identifier.
    pub operation_id: Uuid,
    /// Target instance.
    pub instance_id: Uuid,
    /// Structural revision observed by the editor.
    pub base_structure_revision: u64,
    /// Operation to apply atomically.
    pub operation: StructureOperationKind,
}

/// Latest materialized state of one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    /// Target item.
    pub item_id: Uuid,
    /// Whether the latest valid action completed the item.
    pub completed: bool,
    /// Pubkey of the latest actor.
    pub actor_pubkey: String,
    /// Canonical relay acceptance time.
    pub accepted_at: DateTime<Utc>,
    /// Advisory timestamp from a completion-state action, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_created_at: Option<DateTime<Utc>>,
    /// Source signed event identifier.
    pub event_id: String,
}

/// Fold append-only item actions into one deterministic latest state per item.
#[must_use]
pub fn fold_item_states(actions: impl IntoIterator<Item = ItemState>) -> Vec<ItemState> {
    let mut latest = HashMap::<Uuid, ItemState>::new();
    for action in actions {
        let replace = latest.get(&action.item_id).is_none_or(|current| {
            (action.accepted_at, action.event_id.as_str())
                > (current.accepted_at, current.event_id.as_str())
        });
        if replace {
            latest.insert(action.item_id, action);
        }
    }
    let mut states: Vec<_> = latest.into_values().collect();
    states.sort_by_key(|state| state.item_id);
    states
}

/// One immutable activity record in a relay-signed instance snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Source signed event identifier.
    pub event_id: String,
    /// Actor pubkey.
    pub actor_pubkey: String,
    /// Canonical relay acceptance time.
    pub accepted_at: DateTime<Utc>,
    /// Advisory timestamp from a completion-state action, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_created_at: Option<DateTime<Utc>>,
    /// Action or operation name.
    pub action: String,
    /// Optional item target.
    #[serde(default)]
    pub item_id: Option<Uuid>,
    /// Optional structural revision produced by the operation.
    #[serde(default)]
    pub structure_revision: Option<u64>,
}

/// Relay-signed read projection for a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSnapshot {
    /// Current template definition.
    pub template: TemplateRevision,
    /// Pubkey that authored the current revision.
    pub actor_pubkey: String,
    /// Canonical relay acceptance time.
    pub accepted_at: DateTime<Utc>,
    /// Source event identifier.
    pub event_id: String,
}

/// Relay-signed read projection for a channel instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    /// Current structural definition.
    pub instance: PlaybookInstance,
    /// Latest item states.
    pub item_states: Vec<ItemState>,
    /// Complete append-only activity history.
    pub activity: Vec<ActivityEntry>,
}

/// Structural projection error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaybookError {
    /// Unsupported schema version.
    #[error("unsupported schema_version {0}")]
    SchemaVersion(u32),
    /// Invalid or empty text field.
    #[error("invalid {0}")]
    InvalidText(&'static str),
    /// Duplicate stable identifier.
    #[error("duplicate {0} id {1}")]
    DuplicateId(&'static str, Uuid),
    /// Sparse ordering position must be positive.
    #[error("invalid position {0}")]
    InvalidPosition(i64),
    /// Target section was absent.
    #[error("section not found: {0}")]
    SectionNotFound(Uuid),
    /// Target item was absent.
    #[error("item not found: {0}")]
    ItemNotFound(Uuid),
    /// Target has been tombstoned.
    #[error("{0} is removed")]
    Removed(&'static str),
    /// Target is already active.
    #[error("{0} is already active")]
    AlreadyActive(&'static str),
    /// Archived instances are immutable.
    #[error("instance is archived")]
    Archived,
}

fn validate_text(value: &str, field: &'static str) -> Result<(), PlaybookError> {
    if value.trim().is_empty() {
        Err(PlaybookError::InvalidText(field))
    } else {
        Ok(())
    }
}

/// Validate section shape and stable identifier uniqueness.
pub fn validate_sections(sections: &[PlaybookSection]) -> Result<(), PlaybookError> {
    let mut section_ids = HashSet::new();
    let mut item_ids = HashSet::new();
    for section in sections {
        if !section_ids.insert(section.section_id) {
            return Err(PlaybookError::DuplicateId("section", section.section_id));
        }
        validate_text(&section.title, "section title")?;
        if section.position <= 0 {
            return Err(PlaybookError::InvalidPosition(section.position));
        }
        for item in &section.items {
            if !item_ids.insert(item.item_id) {
                return Err(PlaybookError::DuplicateId("item", item.item_id));
            }
            validate_text(&item.text, "item text")?;
            if item.position <= 0 {
                return Err(PlaybookError::InvalidPosition(item.position));
            }
        }
    }
    Ok(())
}

impl TemplateRevision {
    /// Validate schema, revision, text, positions, and identifiers.
    pub fn validate(&self) -> Result<(), PlaybookError> {
        if self.schema_version != PLAYBOOK_SCHEMA_VERSION {
            return Err(PlaybookError::SchemaVersion(self.schema_version));
        }
        if self.revision == 0 {
            return Err(PlaybookError::InvalidPosition(0));
        }
        validate_text(&self.name, "template name")?;
        validate_sections(&self.sections)
    }
}

impl PlaybookInstance {
    /// Create an independent instance by deep-copying a template revision.
    #[must_use]
    pub fn from_template(instance_id: Uuid, channel_id: Uuid, source: &TemplateRevision) -> Self {
        Self {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            instance_id,
            channel_id,
            name: source.name.clone(),
            source_template_id: source.template_id,
            source_template_revision: source.revision,
            structure_revision: 1,
            status: InstanceStatus::Active,
            sections: source.sections.clone(),
        }
    }

    /// Return active completed-count and active total-count.
    #[must_use]
    pub fn progress(&self, states: &[ItemState]) -> (usize, usize) {
        let completed: HashSet<Uuid> = states
            .iter()
            .filter(|state| state.completed)
            .map(|state| state.item_id)
            .collect();
        let active_items = self
            .sections
            .iter()
            .filter(|section| !section.deleted)
            .flat_map(|section| section.items.iter())
            .filter(|item| !item.deleted);
        active_items.fold((0, 0), |(done, total), item| {
            (
                done + usize::from(completed.contains(&item.item_id)),
                total + 1,
            )
        })
    }

    /// Return whether an active item exists.
    #[must_use]
    pub fn has_active_item(&self, item_id: Uuid) -> bool {
        self.sections.iter().any(|section| {
            !section.deleted
                && section
                    .items
                    .iter()
                    .any(|item| item.item_id == item_id && !item.deleted)
        })
    }

    /// Apply a validated structural operation without incrementing the revision.
    pub fn apply_operation(
        &mut self,
        operation: &StructureOperationKind,
    ) -> Result<(), PlaybookError> {
        if self.status == InstanceStatus::Archived {
            return Err(PlaybookError::Archived);
        }
        match operation {
            StructureOperationKind::SectionAdd { section } => {
                let mut candidate = self.sections.clone();
                candidate.push(section.clone());
                validate_sections(&candidate)?;
                self.sections.push(section.clone());
            }
            StructureOperationKind::SectionUpdate { section_id, title } => {
                validate_text(title, "section title")?;
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.section_id == *section_id)
                    .ok_or(PlaybookError::SectionNotFound(*section_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                section.title.clone_from(title);
            }
            StructureOperationKind::SectionRemove { section_id } => {
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.section_id == *section_id)
                    .ok_or(PlaybookError::SectionNotFound(*section_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                section.deleted = true;
            }
            StructureOperationKind::SectionRestore { section_id } => {
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.section_id == *section_id)
                    .ok_or(PlaybookError::SectionNotFound(*section_id))?;
                if !section.deleted {
                    return Err(PlaybookError::AlreadyActive("section"));
                }
                section.deleted = false;
            }
            StructureOperationKind::SectionReorder {
                section_id,
                position,
            } => {
                if *position <= 0 {
                    return Err(PlaybookError::InvalidPosition(*position));
                }
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.section_id == *section_id)
                    .ok_or(PlaybookError::SectionNotFound(*section_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                section.position = *position;
            }
            StructureOperationKind::ItemAdd { section_id, item } => {
                validate_text(&item.text, "item text")?;
                if item.position <= 0 {
                    return Err(PlaybookError::InvalidPosition(item.position));
                }
                if self
                    .sections
                    .iter()
                    .flat_map(|section| &section.items)
                    .any(|existing| existing.item_id == item.item_id)
                {
                    return Err(PlaybookError::DuplicateId("item", item.item_id));
                }
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.section_id == *section_id)
                    .ok_or(PlaybookError::SectionNotFound(*section_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                section.items.push(item.clone());
            }
            StructureOperationKind::ItemUpdate { item_id, text } => {
                validate_text(text, "item text")?;
                let item = find_active_section_item_mut(&mut self.sections, *item_id)?;
                if item.deleted {
                    return Err(PlaybookError::Removed("item"));
                }
                item.text.clone_from(text);
            }
            StructureOperationKind::ItemRemove { item_id } => {
                let item = find_active_section_item_mut(&mut self.sections, *item_id)?;
                if item.deleted {
                    return Err(PlaybookError::Removed("item"));
                }
                item.deleted = true;
            }
            StructureOperationKind::ItemRestore { item_id } => {
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.items.iter().any(|item| item.item_id == *item_id))
                    .ok_or(PlaybookError::ItemNotFound(*item_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                let item = section
                    .items
                    .iter_mut()
                    .find(|item| item.item_id == *item_id)
                    .ok_or(PlaybookError::ItemNotFound(*item_id))?;
                if !item.deleted {
                    return Err(PlaybookError::AlreadyActive("item"));
                }
                item.deleted = false;
            }
            StructureOperationKind::ItemReorder { item_id, position } => {
                if *position <= 0 {
                    return Err(PlaybookError::InvalidPosition(*position));
                }
                let item = find_active_section_item_mut(&mut self.sections, *item_id)?;
                if item.deleted {
                    return Err(PlaybookError::Removed("item"));
                }
                item.position = *position;
            }
            StructureOperationKind::ItemDuplicate {
                item_id,
                new_item_id,
                position,
            } => {
                if *position <= 0 {
                    return Err(PlaybookError::InvalidPosition(*position));
                }
                if self
                    .sections
                    .iter()
                    .flat_map(|section| &section.items)
                    .any(|item| item.item_id == *new_item_id)
                {
                    return Err(PlaybookError::DuplicateId("item", *new_item_id));
                }
                let section = self
                    .sections
                    .iter_mut()
                    .find(|section| section.items.iter().any(|item| item.item_id == *item_id))
                    .ok_or(PlaybookError::ItemNotFound(*item_id))?;
                if section.deleted {
                    return Err(PlaybookError::Removed("section"));
                }
                let mut copy = section
                    .items
                    .iter()
                    .find(|item| item.item_id == *item_id)
                    .ok_or(PlaybookError::ItemNotFound(*item_id))?
                    .clone();
                if copy.deleted {
                    return Err(PlaybookError::Removed("item"));
                }
                copy.item_id = *new_item_id;
                copy.position = *position;
                copy.deleted = false;
                section.items.push(copy);
            }
            StructureOperationKind::InstanceRename { name } => {
                validate_text(name, "instance name")?;
                self.name.clone_from(name);
            }
            StructureOperationKind::InstanceArchive => {
                self.status = InstanceStatus::Archived;
            }
        }
        Ok(())
    }
}

fn find_active_section_item_mut(
    sections: &mut [PlaybookSection],
    item_id: Uuid,
) -> Result<&mut PlaybookItem, PlaybookError> {
    let section = sections
        .iter_mut()
        .find(|section| section.items.iter().any(|item| item.item_id == item_id))
        .ok_or(PlaybookError::ItemNotFound(item_id))?;
    if section.deleted {
        return Err(PlaybookError::Removed("section"));
    }
    section
        .items
        .iter_mut()
        .find(|item| item.item_id == item_id)
        .ok_or(PlaybookError::ItemNotFound(item_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> TemplateRevision {
        TemplateRevision {
            schema_version: 1,
            template_id: Uuid::new_v4(),
            revision: 1,
            name: "Launch".into(),
            description: None,
            status: TemplateStatus::Active,
            sections: vec![PlaybookSection {
                section_id: Uuid::new_v4(),
                title: "Discovery".into(),
                position: 1000,
                deleted: false,
                items: vec![PlaybookItem {
                    item_id: Uuid::new_v4(),
                    text: "Collect access".into(),
                    position: 1000,
                    deleted: false,
                }],
            }],
        }
    }

    #[test]
    fn deep_copy_isolation() {
        let source = template();
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance.sections[0].items[0].text = "Changed".into();
        assert_eq!(source.sections[0].items[0].text, "Collect access");
    }

    #[test]
    fn duplicate_item_ids_are_rejected_across_sections() {
        let mut value = template();
        let duplicate = value.sections[0].items[0].clone();
        value.sections.push(PlaybookSection {
            section_id: Uuid::new_v4(),
            title: "Other".into(),
            position: 2000,
            items: vec![duplicate],
            deleted: false,
        });
        assert!(matches!(
            value.validate(),
            Err(PlaybookError::DuplicateId("item", _))
        ));
    }

    #[test]
    fn tombstone_and_restore_reuse_item_id() {
        let source = template();
        let item_id = source.sections[0].items[0].item_id;
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance
            .apply_operation(&StructureOperationKind::ItemRemove { item_id })
            .unwrap();
        assert!(!instance.has_active_item(item_id));
        instance
            .apply_operation(&StructureOperationKind::ItemRestore { item_id })
            .unwrap();
        assert!(instance.has_active_item(item_id));
    }

    #[test]
    fn restore_item_under_removed_section_is_rejected() {
        let source = template();
        let section_id = source.sections[0].section_id;
        let item_id = source.sections[0].items[0].item_id;
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance
            .apply_operation(&StructureOperationKind::ItemRemove { item_id })
            .unwrap();
        instance
            .apply_operation(&StructureOperationKind::SectionRemove { section_id })
            .unwrap();
        assert_eq!(
            instance.apply_operation(&StructureOperationKind::ItemRestore { item_id }),
            Err(PlaybookError::Removed("section"))
        );
    }

    #[test]
    fn edits_to_items_under_removed_sections_are_rejected() {
        let source = template();
        let section_id = source.sections[0].section_id;
        let item_id = source.sections[0].items[0].item_id;
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance
            .apply_operation(&StructureOperationKind::SectionRemove { section_id })
            .unwrap();

        for operation in [
            StructureOperationKind::ItemUpdate {
                item_id,
                text: "Hidden edit".into(),
            },
            StructureOperationKind::ItemRemove { item_id },
            StructureOperationKind::ItemReorder {
                item_id,
                position: 2000,
            },
        ] {
            assert_eq!(
                instance.apply_operation(&operation),
                Err(PlaybookError::Removed("section"))
            );
        }
        assert_eq!(instance.sections[0].items[0].text, "Collect access");
        assert_eq!(instance.sections[0].items[0].position, 1000);
        assert!(!instance.sections[0].items[0].deleted);
    }

    #[test]
    fn progress_ignores_tombstones_and_handles_zero_items() {
        let source = template();
        let item_id = source.sections[0].items[0].item_id;
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        let states = vec![ItemState {
            item_id,
            completed: true,
            actor_pubkey: "a".repeat(64),
            accepted_at: Utc::now(),
            client_created_at: None,
            event_id: "b".repeat(64),
        }];
        assert_eq!(instance.progress(&states), (1, 1));
        instance.sections[0].items[0].deleted = true;
        assert_eq!(instance.progress(&states), (0, 0));
    }

    #[test]
    fn archived_instance_rejects_further_edits() {
        let source = template();
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance
            .apply_operation(&StructureOperationKind::InstanceArchive)
            .unwrap();
        assert_eq!(
            instance.apply_operation(&StructureOperationKind::InstanceRename {
                name: "Nope".into()
            }),
            Err(PlaybookError::Archived)
        );
    }

    #[test]
    fn operation_wire_names_are_stable() {
        let value = serde_json::to_value(StructureOperationKind::InstanceArchive).unwrap();
        assert_eq!(value["type"], "instance.archive");
    }

    #[test]
    fn duplicate_item_uses_fresh_id_and_independent_position() {
        let source = template();
        let item_id = source.sections[0].items[0].item_id;
        let new_item_id = Uuid::new_v4();
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);
        instance
            .apply_operation(&StructureOperationKind::ItemDuplicate {
                item_id,
                new_item_id,
                position: 2000,
            })
            .unwrap();
        assert_eq!(instance.sections[0].items.len(), 2);
        assert_eq!(instance.sections[0].items[1].item_id, new_item_id);
        assert_eq!(instance.sections[0].items[1].text, "Collect access");
        assert_eq!(instance.sections[0].items[1].position, 2000);
    }

    #[test]
    fn every_structural_operation_applies_with_stable_tombstones_and_ids() {
        let source = template();
        let original_section_id = source.sections[0].section_id;
        let original_item_id = source.sections[0].items[0].item_id;
        let added_section_id = Uuid::new_v4();
        let added_item_id = Uuid::new_v4();
        let duplicated_item_id = Uuid::new_v4();
        let mut instance = PlaybookInstance::from_template(Uuid::new_v4(), Uuid::new_v4(), &source);

        let operations = [
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
                new_item_id: duplicated_item_id,
                position: 3000,
            },
            StructureOperationKind::InstanceRename {
                name: "Channel launch".into(),
            },
        ];
        for operation in operations {
            instance.apply_operation(&operation).unwrap();
        }

        let added_section = instance
            .sections
            .iter()
            .find(|section| section.section_id == added_section_id)
            .unwrap();
        assert_eq!(added_section.title, "Launch day");
        assert_eq!(added_section.position, 500);
        assert!(!added_section.deleted);
        let added_item = instance
            .sections
            .iter()
            .flat_map(|section| &section.items)
            .find(|item| item.item_id == added_item_id)
            .unwrap();
        assert_eq!(added_item.text, "Confirm launch owner");
        assert_eq!(added_item.position, 500);
        assert!(!added_item.deleted);
        assert!(instance
            .sections
            .iter()
            .flat_map(|section| &section.items)
            .any(|item| item.item_id == duplicated_item_id));
        assert_eq!(instance.name, "Channel launch");

        instance
            .apply_operation(&StructureOperationKind::InstanceArchive)
            .unwrap();
        assert_eq!(instance.status, InstanceStatus::Archived);
    }

    #[test]
    fn structural_operation_wire_shapes_round_trip_and_fail_closed() {
        let section_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let variants = vec![
            StructureOperationKind::SectionAdd {
                section: PlaybookSection {
                    section_id,
                    title: "Section".into(),
                    position: 1000,
                    deleted: false,
                    items: vec![],
                },
            },
            StructureOperationKind::SectionUpdate {
                section_id,
                title: "Renamed".into(),
            },
            StructureOperationKind::SectionRemove { section_id },
            StructureOperationKind::SectionRestore { section_id },
            StructureOperationKind::SectionReorder {
                section_id,
                position: 2000,
            },
            StructureOperationKind::ItemAdd {
                section_id,
                item: PlaybookItem {
                    item_id,
                    text: "Item".into(),
                    position: 1000,
                    deleted: false,
                },
            },
            StructureOperationKind::ItemUpdate {
                item_id,
                text: "Updated".into(),
            },
            StructureOperationKind::ItemRemove { item_id },
            StructureOperationKind::ItemRestore { item_id },
            StructureOperationKind::ItemReorder {
                item_id,
                position: 2000,
            },
            StructureOperationKind::ItemDuplicate {
                item_id,
                new_item_id: Uuid::new_v4(),
                position: 3000,
            },
            StructureOperationKind::InstanceRename {
                name: "Renamed".into(),
            },
            StructureOperationKind::InstanceArchive,
        ];
        for variant in variants {
            let encoded = serde_json::to_value(&variant).unwrap();
            let decoded: StructureOperationKind = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, variant);
        }

        assert!(
            serde_json::from_value::<StructureOperationKind>(serde_json::json!({
                "type": "item.unknown",
                "payload": {"item_id": item_id}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<StructureOperationKind>(serde_json::json!({
                "type": "item.reorder",
                "payload": {"item_id": item_id}
            }))
            .is_err()
        );
    }

    #[test]
    fn malformed_template_shapes_fail_closed() {
        let mut value = template();
        value.schema_version = 2;
        assert_eq!(value.validate(), Err(PlaybookError::SchemaVersion(2)));

        let mut value = template();
        value.sections[0].items[0].position = 0;
        assert_eq!(value.validate(), Err(PlaybookError::InvalidPosition(0)));

        let mut value = template();
        value.name = "  ".into();
        assert_eq!(
            value.validate(),
            Err(PlaybookError::InvalidText("template name"))
        );
    }

    #[test]
    fn item_state_fold_is_permutation_independent_with_event_id_tiebreak() {
        let item_id = Uuid::new_v4();
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let complete = ItemState {
            item_id,
            completed: true,
            actor_pubkey: "a".repeat(64),
            accepted_at: at,
            client_created_at: None,
            event_id: "1".repeat(64),
        };
        let reopen = ItemState {
            completed: false,
            actor_pubkey: "b".repeat(64),
            event_id: "f".repeat(64),
            ..complete.clone()
        };
        let forward = fold_item_states([complete.clone(), reopen.clone()]);
        let reverse = fold_item_states([reopen, complete]);
        assert_eq!(forward, reverse);
        assert!(!forward[0].completed);
    }
}
