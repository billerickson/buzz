#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

relay_url="${BUZZ_RELAY_URL:-http://127.0.0.1:3000}"
if [[ "${relay_url}" != "http://127.0.0.1:3000" ]]; then
  echo "error: refusing to run against ${relay_url}; this fixture only targets http://127.0.0.1:3000" >&2
  exit 1
fi

for command in cargo curl jq uuidgen; do
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "error: required command not found: ${command}" >&2
    exit 1
  fi
done

buzz_bin="${repo_root}/target/debug/buzz"
if [[ ! -x "${buzz_bin}" ]]; then
  echo "error: ${buzz_bin} is missing; run 'cargo build -p buzz-cli' first" >&2
  exit 1
fi

# Public, repository-owned Tyler development identity. It is intentionally
# unsuitable for any hosted or non-loopback relay.
export BUZZ_RELAY_URL="${relay_url}"
export BUZZ_PRIVATE_KEY="3dbaebadb5dfd777ff25149ee230d907a15a9e1294b40b830661e65bb42f6c03"
unset BUZZ_AUTH_TAG

fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/buzz-playbooks-smoke.XXXXXX")"
cleanup() {
  rm -rf "${fixture_dir}"
}
trap cleanup EXIT

new_uuid() {
  uuidgen | tr '[:upper:]' '[:lower:]'
}

template_id="$(new_uuid)"
section_id="$(new_uuid)"
item_id="$(new_uuid)"
added_item_id="$(new_uuid)"
check_action_id="$(new_uuid)"
check_client_created_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
channel_name="playbooks-smoke-$(date +%s)"

cat >"${fixture_dir}/template-v1.json" <<JSON
{
  "schema_version": 1,
  "template_id": "${template_id}",
  "revision": 1,
  "name": "Cultivate Go smoke fixture",
  "description": "Disposable local Phase 1 fixture",
  "status": "active",
  "sections": [
    {
      "section_id": "${section_id}",
      "title": "Discovery",
      "position": 1000,
      "items": [
        {
          "item_id": "${item_id}",
          "text": "Collect analytics access",
          "position": 1000
        }
      ]
    }
  ]
}
JSON

cat >"${fixture_dir}/template-v2.json" <<JSON
{
  "schema_version": 1,
  "template_id": "${template_id}",
  "revision": 2,
  "name": "Cultivate Go smoke fixture v2",
  "description": "Existing instances must remain on revision 1",
  "status": "active",
  "sections": [
    {
      "section_id": "${section_id}",
      "title": "Discovery",
      "position": 1000,
      "items": [
        {
          "item_id": "${item_id}",
          "text": "Collect revised analytics access",
          "position": 1000
        }
      ]
    }
  ]
}
JSON

echo "Checking local relay..."
curl --silent --fail --max-time 2 "${relay_url}/health" >/dev/null

channel_json="$("${buzz_bin}" channels create \
  --name "${channel_name}" \
  --type stream \
  --visibility open)"
channel_id="$(jq -er '.channel_id' <<<"${channel_json}")"

template_create="$("${buzz_bin}" playbooks templates create \
  --file "${fixture_dir}/template-v1.json")"
jq -e --arg id "${template_id}" \
  '.accepted == true and .template_id == $id' <<<"${template_create}" >/dev/null

template_get="$("${buzz_bin}" playbooks templates get --template "${template_id}")"
jq -e '.template.revision == 1 and .template.status == "active"' \
  <<<"${template_get}" >/dev/null
template_list="$("${buzz_bin}" playbooks templates list)"
jq -e --arg id "${template_id}" \
  'any(.[]; .template.template_id == $id)' <<<"${template_list}" >/dev/null

insert_json="$("${buzz_bin}" playbooks insert \
  --channel "${channel_id}" \
  --template "${template_id}")"
instance_id="$(jq -er '.instance_id' <<<"${insert_json}")"

instance_v1="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e \
  --arg template "${template_id}" \
  --arg item "${item_id}" \
  '.instance.source_template_id == $template
    and .instance.source_template_revision == 1
    and .instance.sections[0].items[0].item_id == $item
    and .instance.sections[0].items[0].text == "Collect analytics access"' \
  <<<"${instance_v1}" >/dev/null

check_first="$("${buzz_bin}" playbooks check \
  --instance "${instance_id}" \
  --item "${item_id}" \
  --action-id "${check_action_id}" \
  --client-created-at "${check_client_created_at}")"
check_event_id="$(jq -er '.event_id' <<<"${check_first}")"
check_canonical_event_id="$(jq -er '.canonical_event_id' <<<"${check_first}")"
jq -e \
  --arg event "${check_event_id}" \
  '.accepted == true and .canonical_event_id == $event' \
  <<<"${check_first}" >/dev/null
checked="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e --arg item "${item_id}" \
  '.item_states[] | select(.item_id == $item and .completed == true)' \
  <<<"${checked}" >/dev/null

sleep 1
check_retry="$("${buzz_bin}" playbooks check \
  --instance "${instance_id}" \
  --item "${item_id}" \
  --action-id "${check_action_id}" \
  --client-created-at "${check_client_created_at}")"
jq -e \
  --arg canonical "${check_canonical_event_id}" \
  '.accepted == true
    and .event_id != $canonical
    and .canonical_event_id == $canonical' \
  <<<"${check_retry}" >/dev/null

"${buzz_bin}" playbooks reopen --instance "${instance_id}" --item "${item_id}" >/dev/null
reopened="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e --arg item "${item_id}" \
  '.item_states[] | select(.item_id == $item and .completed == false)' \
  <<<"${reopened}" >/dev/null

cat >"${fixture_dir}/item-add.json" <<JSON
{
  "schema_version": 1,
  "operation_id": "$(new_uuid)",
  "instance_id": "${instance_id}",
  "base_structure_revision": 1,
  "operation": {
    "type": "item.add",
    "payload": {
      "section_id": "${section_id}",
      "item": {
        "item_id": "${added_item_id}",
        "text": "Confirm launch owner",
        "position": 2000
      }
    }
  }
}
JSON

edit_first="$("${buzz_bin}" playbooks edit \
  --instance "${instance_id}" \
  --operation-file "${fixture_dir}/item-add.json")"
edit_event_id="$(jq -er '.event_id' <<<"${edit_first}")"
edit_canonical_event_id="$(jq -er '.canonical_event_id' <<<"${edit_first}")"
jq -e \
  --arg event "${edit_event_id}" \
  '.accepted == true and .canonical_event_id == $event' \
  <<<"${edit_first}" >/dev/null
edited="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e --arg item "${added_item_id}" \
  '.instance.structure_revision == 2
    and any(.instance.sections[].items[]; .item_id == $item and .deleted == false)' \
  <<<"${edited}" >/dev/null

sleep 1
edit_retry="$("${buzz_bin}" playbooks edit \
  --instance "${instance_id}" \
  --operation-file "${fixture_dir}/item-add.json")"
jq -e \
  --arg canonical "${edit_canonical_event_id}" \
  '.accepted == true
    and .event_id != $canonical
    and .canonical_event_id == $canonical' \
  <<<"${edit_retry}" >/dev/null
after_edit_retry="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e --arg item "${added_item_id}" \
  '.instance.structure_revision == 2
    and ([.instance.sections[].items[] | select(.item_id == $item)] | length) == 1
    and ([.activity[] | select(.action == "item.add")] | length) == 1' \
  <<<"${after_edit_retry}" >/dev/null

"${buzz_bin}" playbooks templates update \
  --template "${template_id}" \
  --file "${fixture_dir}/template-v2.json" >/dev/null
template_v2="$("${buzz_bin}" playbooks templates get --template "${template_id}")"
jq -e '.template.revision == 2' <<<"${template_v2}" >/dev/null

isolated="$("${buzz_bin}" playbooks get --instance "${instance_id}")"
jq -e \
  '.instance.source_template_revision == 1
    and .instance.sections[0].items[0].text == "Collect analytics access"' \
  <<<"${isolated}" >/dev/null

activity="$("${buzz_bin}" playbooks activity --instance "${instance_id}")"
jq -e \
  'map(.action) as $actions
    | ($actions | index("instance.insert")) != null
    and ($actions | index("complete")) != null
    and ($actions | index("reopen")) != null
    and ($actions | index("item.add")) != null' \
  <<<"${activity}" >/dev/null
item_command_events="$("${buzz_bin}" messages get \
  --channel "${channel_id}" \
  --kinds 40202)"
jq -e 'length == 2' <<<"${item_command_events}" >/dev/null
structure_command_events="$("${buzz_bin}" messages get \
  --channel "${channel_id}" \
  --kinds 40203)"
jq -e 'length == 1' <<<"${structure_command_events}" >/dev/null

RELAY_URL=ws://127.0.0.1:3000 \
BUZZ_TEST_OWNER_PRIVATE_KEY="${BUZZ_PRIVATE_KEY}" \
  cargo test -p buzz-test-client --test e2e_relay \
    test_playbook_semantic_retries_echo_wrapper_and_suppress_fanout \
    -- --ignored --exact

cat <<SUMMARY
Playbooks Phase 1 smoke test passed.
  relay:       ${relay_url}
  channel:     ${channel_id}
  template:    ${template_id} (current revision 2)
  instance:    ${instance_id} (isolated revision 1 copy)
  item:        ${item_id} (wrapper/canonical IDs + complete/reopen history verified)
  added item:  ${added_item_id} (semantic edit retry stored once)
  WebSocket:   wrapper-correlated retries + no second fanout verified
SUMMARY
