#!/usr/bin/env bash
set -euo pipefail
umask 077

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly workspace
readonly evidence="$workspace/tools/linux-docker/evidence.py"
readonly source_digest="$workspace/tools/linux-docker/source_digest.py"
readonly bounded_runner="$workspace/tools/linux-docker/bounded_runner.py"
platforms=(linux/arm64)
output_root=""
expected_source=""
base_image=""
docker_host=""
acknowledged=false
output_initialized=false
cleanup_report_written=false
command_report_written=false
active_builder_name=""
active_builder_identity=""
active_builder_created=false
active_builder_attempted=false
active_scope="host"
active_label_prefix="host"
active_container_name=""
active_container_id=""
active_container_created=false
active_container_attempted=false
active_image_tag=""
active_image_id=""
active_image_created=false
active_image_attempted=false
created_builder_count=0
removed_builder_count=0
created_container_count=0
removed_container_count=0
loaded_image_count=0
removed_image_count=0
unconfirmed_builder_count=0
unconfirmed_container_count=0
unconfirmed_image_count=0
active_bounded_runner_pid=""
requested_signal_status=0

usage() {
  cat >&2 <<'USAGE'
usage: tools/linux-docker/run.sh \
  --source-sha256 FROZEN_SHA256 \
  --base-image docker.io/library/rust:1.88.0-bookworm@sha256:MULTIARCH_DIGEST \
  --acknowledge-docker \
  [--platform arm64|amd64|all] \
  [--docker-host unix:///ABSOLUTE/LOCAL/SOCKET] \
  [--output ABSOLUTE_EMPTY_DIR]

The acknowledgement permits this runner to create and exactly remove its own
per-cell Buildx builders, containers, and image tags. It never prunes or
removes unrelated Docker state.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --platform)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      case "$2" in
        arm64) platforms=(linux/arm64) ;;
        amd64) platforms=(linux/amd64) ;;
        all) platforms=(linux/arm64 linux/amd64) ;;
        *) usage; exit 2 ;;
      esac
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      output_root="$2"
      shift 2
      ;;
    --source-sha256)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      expected_source="$2"
      shift 2
      ;;
    --base-image)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      base_image="$2"
      shift 2
      ;;
    --docker-host)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      docker_host="$2"
      shift 2
      ;;
    --acknowledge-docker)
      acknowledged=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

if [[ ! "$expected_source" =~ ^[0-9a-f]{64}$ ]]; then
  echo "linux-docker: --source-sha256 must be the exact lowercase frozen digest" >&2
  exit 2
fi
if [[ "$acknowledged" != true ]]; then
  echo "linux-docker: --acknowledge-docker is required before creating Docker objects" >&2
  exit 2
fi
if ! base_image="$("$evidence" base-image "$base_image")"; then
  exit 2
fi
readonly base_image
if [[ -n "$output_root" && "$output_root" != /* ]]; then
  echo "linux-docker: --output must be absolute" >&2
  exit 2
fi
if [[ -z "$docker_host" ]]; then
  if [[ -n "${DOCKER_HOST:-}" ]]; then
    docker_host="$DOCKER_HOST"
  elif [[ -n "${HOME:-}" && -S "$HOME/.docker/run/docker.sock" ]]; then
    docker_host="unix://$HOME/.docker/run/docker.sock"
  else
    docker_host="unix:///var/run/docker.sock"
  fi
fi
docker_socket_path="${docker_host#unix://}"
if [[ ! "$docker_host" =~ ^unix:///[A-Za-z0-9._/@:+-]+$ \
  || "$docker_socket_path" == *"/../"* || "$docker_socket_path" == *"/./"* \
  || "$docker_socket_path" == *"//"* ]]; then
  echo "linux-docker: --docker-host must identify one canonical local Unix socket" >&2
  exit 2
fi
readonly docker_host
readonly docker_socket_path

# Reject a stale/fabricated freeze before creating an evidence directory or
# touching Docker. The manifest is captured again atomically after the private
# output root exists, closing the check-to-publication interval.
"$source_digest" "$workspace" --expected "$expected_source" >/dev/null

run_id="$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
readonly run_id
if [[ -z "$output_root" ]]; then
  output_root="$workspace/.context/linux-docker/$(date -u +%Y%m%dT%H%M%SZ)-${expected_source:0:12}-$run_id"
fi
readonly output_root
readonly resource_ledger="$output_root/docker-resource-ledger.ndjson"
resource_ledger_ordinal=0
resource_ledger_anchor="START"
readonly command_receipt_root="$output_root/host-command-receipts"
readonly command_ledger="$output_root/bounded-command-ledger.ndjson"
command_ledger_ordinal=0
command_ledger_anchor="START"
last_command_stdout=""
last_command_stderr=""
last_command_receipt=""

"$evidence" prepare-output "$output_root"
output_initialized=true
"$evidence" prepare-output "$command_receipt_root"
readonly docker_home="$output_root/host-docker-home"
readonly docker_config="$output_root/host-docker-config"
readonly buildx_config="$output_root/host-buildx-config"
"$evidence" prepare-output "$docker_home"
"$evidence" prepare-output "$docker_config"
"$evidence" prepare-output "$buildx_config"
readonly docker_transport_before="$output_root/host-docker-transport-before.json"
readonly docker_transport_after="$output_root/host-docker-transport-after.json"
"$evidence" docker-transport "$docker_socket_path" "$docker_transport_before"
docker_executable="$(command -v docker || true)"
if [[ -z "$docker_executable" || "$docker_executable" != /* ]]; then
  echo "linux-docker: docker is unavailable as one absolute executable" >&2
  exit 2
fi
readonly docker_executable
docker_path="$(dirname "$docker_executable"):/usr/local/lib/docker/cli-plugins:/usr/local/bin:/usr/bin:/bin"
readonly docker_path
docker_environment=(
  --env "BUILDX_CONFIG=$buildx_config"
  --env "DOCKER_CONFIG=$docker_config"
  --env "DOCKER_HOST=$docker_host"
  --env "HOME=$docker_home"
  --env 'LANG=C'
  --env 'LC_ALL=C'
  --env "PATH=$docker_path"
  --env 'TZ=UTC'
)
readonly docker_environment
"$source_digest" "$workspace" \
  --expected "$expected_source" \
  --json \
  --output "$output_root/host-source-before.json"
"$source_digest" "$workspace" --revision-capture \
  --output "$output_root/host-revision-capture-before.json"
platforms_json='["linux/arm64"]'
if [[ ${#platforms[@]} -eq 2 ]]; then
  platforms_json='["linux/arm64","linux/amd64"]'
elif [[ "${platforms[0]}" == "linux/amd64" ]]; then
  platforms_json='["linux/amd64"]'
fi
printf '{"schema_version":1,"acknowledge_docker":true,"expected_source_sha256":"%s","base_image":"%s","docker_host":"%s","docker_environment":"private-empty-config-local-unix-v1","dockerfile_frontend":"daemon-bundled-no-external-syntax-image","platforms":%s}\n' \
  "$expected_source" "$base_image" "$docker_host" "$platforms_json" \
  | "$evidence" write-json "$output_root/run-contract.json"

run_docker() {
  local label="$1"
  local scope="$2"
  local timeout_seconds="$3"
  local maximum_output_bytes="$4"
  shift 4
  if [[ ! "$label" =~ ^[a-z0-9][a-z0-9._-]{0,100}$ \
    || ( "$scope" != host && "$scope" != linux/arm64 \
      && "$scope" != linux/amd64 ) \
    || ! "$timeout_seconds" =~ ^[1-9][0-9]*$ \
    || ! "$maximum_output_bytes" =~ ^[1-9][0-9]*$ ]]; then
    echo "linux-docker: bounded Docker command contract is invalid" >&2
    return 125
  fi
  local next_ordinal=$((command_ledger_ordinal + 1))
  local effective_label
  printf -v effective_label 'c%04d.%s' "$next_ordinal" "$label"
  local prefix
  printf -v prefix '%04d-%s' "$next_ordinal" "${label//./-}"
  last_command_stdout="$command_receipt_root/$prefix.stdout"
  last_command_stderr="$command_receipt_root/$prefix.stderr"
  last_command_receipt="$command_receipt_root/$prefix.receipt.json"
  local runner_status=125
  set +e
  "$bounded_runner" \
    --cwd "$workspace" \
    --timeout-seconds "$timeout_seconds" \
    --drain-timeout-seconds 10 \
    --maximum-output-bytes "$maximum_output_bytes" \
    --stdout "$last_command_stdout" \
    --stderr "$last_command_stderr" \
    --receipt "$last_command_receipt" \
    --description "pmux Docker evidence: $effective_label" \
    "${docker_environment[@]}" \
    -- "$docker_executable" "$@" >/dev/null &
  active_bounded_runner_pid=$!
  while :; do
    wait "$active_bounded_runner_pid"
    runner_status=$?
    if ! kill -0 "$active_bounded_runner_pid" 2>/dev/null; then
      break
    fi
  done
  active_bounded_runner_pid=""
  set -e
  if [[ ! -f "$last_command_receipt" || -L "$last_command_receipt" ]]; then
    echo "linux-docker: bounded Docker command did not publish a full receipt: $effective_label" >&2
    return 125
  fi
  local next_anchor
  next_anchor="$(
    "$evidence" append-command \
      "$command_ledger" "$last_command_receipt" "$effective_label" "$scope" \
      --expected-ordinal "$next_ordinal" \
      --expected-prior-sha256 "$command_ledger_anchor"
  )" || {
    echo "linux-docker: bounded Docker receipt could not be chained: $effective_label" >&2
    return 125
  }
  if [[ ! "$next_anchor" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: bounded Docker command returned an invalid ledger anchor" >&2
    return 125
  fi
  command_ledger_ordinal="$next_ordinal"
  command_ledger_anchor="$next_anchor"
  if [[ $requested_signal_status -ne 0 ]]; then
    exit "$requested_signal_status"
  fi
  return "$runner_status"
}

# shellcheck disable=SC2329  # invoked indirectly by the registered signal traps
forward_runner_signal() {
  local signal_name="$1"
  local signal_status="$2"
  requested_signal_status="$signal_status"
  if [[ -n "$active_bounded_runner_pid" ]]; then
    kill -s "$signal_name" "$active_bounded_runner_pid" 2>/dev/null || true
  fi
}

record_resource() {
  local next_ordinal=$((resource_ledger_ordinal + 1))
  local next_anchor
  next_anchor="$(
    "$evidence" append-resource "$resource_ledger" \
      --expected-ordinal "$next_ordinal" \
      --expected-prior-sha256 "$resource_ledger_anchor" \
      "$@"
  )" || return
  if [[ ! "$next_anchor" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: resource ledger returned an invalid external anchor" >&2
    return 1
  fi
  resource_ledger_ordinal="$next_ordinal"
  resource_ledger_anchor="$next_anchor"
}

cleanup_container() {
  if [[ -z "$active_container_name" ]]; then
    return 0
  fi
  if [[ "$active_container_created" != true ]]; then
    local uncertain=false
    if [[ "$active_container_attempted" == true ]]; then
      echo "linux-docker: container creation has no successful exact receipt; refusing discovery or removal" >&2
      record_resource container "$active_container_name" unknown ownership_unconfirmed
      unconfirmed_container_count=$((unconfirmed_container_count + 1))
      uncertain=true
    fi
    active_container_name=""
    active_container_id=""
    active_container_attempted=false
    [[ "$uncertain" == false ]]
    return
  fi
  if [[ ! "$active_container_id" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: owned container is missing its exact creation receipt" >&2
    record_resource container "$active_container_name" unknown cleanup_failed
    return 1
  fi
  local discovered=""
  if run_docker \
    "$active_label_prefix.cleanup-container-inspect" "$active_scope" 60 8388608 \
    container inspect --format '{{.Id}}' "$active_container_name"; then
    discovered="$(<"$last_command_stdout")"
  fi
  if [[ -z "$discovered" ]]; then
    echo "linux-docker: exact container disappeared before cleanup: $active_container_name" >&2
    record_resource container "$active_container_name" "$active_container_id" cleanup_failed
    active_container_name=""
    active_container_id=""
    active_container_created=false
    return 1
  fi
  if [[ "$discovered" != "$active_container_id" ]]; then
    echo "linux-docker: container name now identifies another object; refusing removal" >&2
    record_resource container "$active_container_name" "$active_container_id" cleanup_failed
    return 1
  fi
  if ! run_docker \
    "$active_label_prefix.cleanup-container-remove" "$active_scope" 120 8388608 \
    rm --force "$active_container_id"; then
    echo "linux-docker: failed to remove exact container $active_container_id" >&2
    record_resource container "$active_container_name" "$active_container_id" cleanup_failed
    return 1
  fi
  local inspect_id_status=125
  local inspect_name_status=125
  if run_docker \
    "$active_label_prefix.cleanup-container-post-id" "$active_scope" 60 8388608 \
    container inspect "$active_container_id"; then
    inspect_id_status=0
  else
    inspect_id_status=$?
  fi
  if run_docker \
    "$active_label_prefix.cleanup-container-post-name" "$active_scope" 60 8388608 \
    container inspect "$active_container_name"; then
    inspect_name_status=0
  else
    inspect_name_status=$?
  fi
  if [[ $inspect_id_status -ne 1 || $inspect_name_status -ne 1 ]]; then
    echo "linux-docker: exact container remains after removal" >&2
    record_resource container "$active_container_name" "$active_container_id" cleanup_failed
    return 1
  fi
  record_resource container "$active_container_name" "$active_container_id" removed
  removed_container_count=$((removed_container_count + 1))
  active_container_name=""
  active_container_id=""
  active_container_created=false
  active_container_attempted=false
}

cleanup_builder() {
  if [[ -z "$active_builder_name" ]]; then
    return 0
  fi
  if [[ "$active_builder_created" != true ]]; then
    local uncertain=false
    if [[ "$active_builder_attempted" == true ]]; then
      echo "linux-docker: builder creation has no successful exact identity; refusing discovery or removal" >&2
      record_resource builder "$active_builder_name" unknown ownership_unconfirmed
      unconfirmed_builder_count=$((unconfirmed_builder_count + 1))
      uncertain=true
    fi
    active_builder_name=""
    active_builder_identity=""
    active_builder_attempted=false
    [[ "$uncertain" == false ]]
    return
  fi
  if [[ ! "$active_builder_identity" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: owned builder is missing its exact captured identity" >&2
    record_resource builder "$active_builder_name" unknown cleanup_failed
    return 1
  fi
  local discovered=false
  local node_name="buildx_buildkit_${active_builder_name}0"
  local node_id=""
  if run_docker \
    "$active_label_prefix.cleanup-builder-inspect" "$active_scope" 60 8388608 \
    buildx inspect "$active_builder_name"; then
    discovered=true
  fi
  if run_docker \
    "$active_label_prefix.cleanup-builder-node" "$active_scope" 60 8388608 \
    container inspect --format '{{.Id}}' "$node_name"; then
    node_id="$(<"$last_command_stdout")"
  fi
  if [[ "$discovered" != true ]]; then
    echo "linux-docker: exact builder disappeared before cleanup: $active_builder_name" >&2
    if [[ -z "$active_builder_identity" ]]; then
      active_builder_identity=unknown
    fi
    record_resource builder "$active_builder_name" "$active_builder_identity" cleanup_failed
    active_builder_name=""
    active_builder_identity=""
    active_builder_created=false
    active_builder_attempted=false
    return 1
  fi
  if [[ -z "$node_id" || "$node_id" != "$active_builder_identity" ]]; then
    echo "linux-docker: builder node identity changed; refusing removal" >&2
    record_resource builder "$active_builder_name" "$active_builder_identity" cleanup_failed
    return 1
  fi
  if ! run_docker \
    "$active_label_prefix.cleanup-builder-remove" "$active_scope" 120 8388608 \
    buildx rm --force "$active_builder_name"; then
    echo "linux-docker: failed to remove exact per-cell builder $active_builder_name" >&2
    record_resource builder "$active_builder_name" "$active_builder_identity" cleanup_failed
    return 1
  fi
  local builder_status=125
  local node_status=125
  if run_docker \
    "$active_label_prefix.cleanup-builder-post-name" "$active_scope" 60 8388608 \
    buildx inspect "$active_builder_name"; then
    builder_status=0
  else
    builder_status=$?
  fi
  if run_docker \
    "$active_label_prefix.cleanup-builder-post-node" "$active_scope" 60 8388608 \
    container inspect "$node_name"; then
    node_status=0
  else
    node_status=$?
  fi
  if [[ $builder_status -ne 1 || $node_status -ne 1 ]]; then
    echo "linux-docker: failed to remove exact per-cell builder $active_builder_name" >&2
    record_resource builder "$active_builder_name" "$active_builder_identity" cleanup_failed
    return 1
  fi
  record_resource builder "$active_builder_name" "$active_builder_identity" removed
  removed_builder_count=$((removed_builder_count + 1))
  active_builder_name=""
  active_builder_identity=""
  active_builder_created=false
  active_builder_attempted=false
}

cleanup_image() {
  if [[ -z "$active_image_tag" ]]; then
    return 0
  fi
  if [[ "$active_image_created" != true ]]; then
    local uncertain=false
    if [[ "$active_image_attempted" == true ]]; then
      echo "linux-docker: image publication has no successful exact IID receipt; refusing discovery or removal" >&2
      record_resource image "$active_image_tag" unknown ownership_unconfirmed
      unconfirmed_image_count=$((unconfirmed_image_count + 1))
      uncertain=true
    fi
    active_image_tag=""
    active_image_id=""
    active_image_attempted=false
    [[ "$uncertain" == false ]]
    return
  fi
  if [[ ! "$active_image_id" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "linux-docker: owned image is missing its exact IID receipt" >&2
    record_resource image "$active_image_tag" unknown cleanup_failed
    return 1
  fi
  local discovered=""
  if run_docker \
    "$active_label_prefix.cleanup-image-inspect" "$active_scope" 60 8388608 \
    image inspect --format '{{.Id}}' "$active_image_tag"; then
    discovered="$(<"$last_command_stdout")"
  fi
  if [[ -z "$discovered" ]]; then
    echo "linux-docker: exact image tag disappeared before cleanup: $active_image_tag" >&2
    record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
    active_image_tag=""
    active_image_id=""
    active_image_created=false
    return 1
  fi
  if [[ "$discovered" != "$active_image_id" ]]; then
    echo "linux-docker: image tag now identifies another object; refusing removal" >&2
    record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
    return 1
  fi
  if ! run_docker \
    "$active_label_prefix.cleanup-image-remove" "$active_scope" 120 8388608 \
    image rm "$active_image_tag"; then
    echo "linux-docker: failed to remove exact per-cell image tag $active_image_tag" >&2
    record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
    return 1
  fi
  local inspect_tag_status=125
  if run_docker \
    "$active_label_prefix.cleanup-image-post-tag" "$active_scope" 60 8388608 \
    image inspect "$active_image_tag"; then
    inspect_tag_status=0
  else
    inspect_tag_status=$?
  fi
  if [[ $inspect_tag_status -ne 1 ]]; then
    echo "linux-docker: failed to remove exact per-cell image tag $active_image_tag" >&2
    record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
    return 1
  fi
  if ! run_docker \
    "$active_label_prefix.cleanup-image-post-list" "$active_scope" 60 8388608 \
    image ls --all --format '{{.Repository}}:{{.Tag}}'; then
    echo "linux-docker: could not prove the image tag listing after cleanup" >&2
    record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
    return 1
  fi
  local discovered_tag
  while IFS= read -r discovered_tag; do
    if [[ "$discovered_tag" == "$active_image_tag" ]]; then
      echo "linux-docker: exact image tag remains after cleanup: $active_image_tag" >&2
      record_resource image "$active_image_tag" "$active_image_id" cleanup_failed
      return 1
    fi
  done <"$last_command_stdout"
  record_resource image "$active_image_tag" "$active_image_id" removed
  removed_image_count=$((removed_image_count + 1))
  active_image_tag=""
  active_image_id=""
  active_image_created=false
  active_image_attempted=false
}

# shellcheck disable=SC2329  # invoked by the EXIT-trap cleanup graph
write_cleanup_report() {
  local exact=true
  local write_status=0
  if [[ $created_builder_count -ne $removed_builder_count \
    || $created_container_count -ne $removed_container_count \
    || $loaded_image_count -ne $removed_image_count \
    || $unconfirmed_builder_count -ne 0 \
    || $unconfirmed_container_count -ne 0 \
    || $unconfirmed_image_count -ne 0 \
    || -n "$active_builder_name" \
    || -n "$active_container_name" \
    || -n "$active_image_tag" ]]; then
    exact=false
  fi
  printf '{"schema_version":1,"exact_cleanup":%s,"created_builders":%s,"removed_builders":%s,"created_containers":%s,"removed_containers":%s,"loaded_images":%s,"removed_images":%s,"unconfirmed_builders":%s,"unconfirmed_containers":%s,"unconfirmed_images":%s,"active_builder":"%s","active_container":"%s","active_image":"%s"}\n' \
    "$exact" \
    "$created_builder_count" "$removed_builder_count" \
    "$created_container_count" "$removed_container_count" \
    "$loaded_image_count" "$removed_image_count" \
    "$unconfirmed_builder_count" "$unconfirmed_container_count" \
    "$unconfirmed_image_count" \
    "$active_builder_name" "$active_container_name" "$active_image_tag" \
    | "$evidence" write-json "$output_root/host-docker-cleanup.json" \
    || write_status=$?
  if [[ $write_status -eq 0 ]]; then
    cleanup_report_written=true
  fi
  [[ "$exact" == true && $write_status -eq 0 ]]
}

# shellcheck disable=SC2329  # invoked by normal and trap finalization paths
write_command_report() {
  if [[ "$command_report_written" == true ]]; then
    return 0
  fi
  if [[ $command_ledger_ordinal -lt 1 \
    || ! "$command_ledger_anchor" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: no complete bounded Docker command ledger is available" >&2
    return 1
  fi
  "$evidence" command-ledger-report \
    "$command_ledger" "$command_ledger_ordinal" "$command_ledger_anchor" \
    "$output_root/bounded-command-ledger-report.json"
  command_report_written=true
}

# shellcheck disable=SC2329  # registered directly as the EXIT trap
cleanup_run() {
  local status=$?
  local cleanup_status=0
  trap - EXIT
  requested_signal_status=0
  cleanup_container || cleanup_status=1
  cleanup_builder || cleanup_status=1
  cleanup_image || cleanup_status=1
  if [[ "$output_initialized" == true ]]; then
    if [[ "$cleanup_report_written" != true ]]; then
      write_cleanup_report || cleanup_status=1
    fi
    if [[ "$command_report_written" != true ]]; then
      write_command_report || cleanup_status=1
    fi
    "$evidence" secure-tree "$output_root" || cleanup_status=1
    if [[ ! -e "$output_root/host-evidence-tree-final.json" \
      && ! -L "$output_root/host-evidence-tree-final.json" ]]; then
      "$evidence" tree-manifest \
        "$output_root" "$output_root/host-evidence-tree-final.json" \
        || cleanup_status=1
    fi
    "$evidence" secure-tree "$output_root" || cleanup_status=1
  fi
  if [[ $cleanup_status -ne 0 ]]; then
    status=1
  fi
  exit "$status"
}

trap cleanup_run EXIT
trap 'forward_runner_signal HUP 129' HUP
trap 'forward_runner_signal INT 130' INT
trap 'forward_runner_signal QUIT 131' QUIT
trap 'forward_runner_signal TERM 143' TERM

if ! run_docker host.docker-version host 30 1048576 version; then
  echo "linux-docker: bounded Docker version query failed" >&2
  exit 2
fi
readonly docker_version_stdout="$last_command_stdout"
readonly docker_version_stderr="$last_command_stderr"
readonly docker_version_receipt="$last_command_receipt"
if ! run_docker host.buildx-version host 30 1048576 buildx version; then
  echo "linux-docker: bounded Buildx version query failed" >&2
  exit 2
fi
readonly buildx_version_stdout="$last_command_stdout"
readonly buildx_version_stderr="$last_command_stderr"
readonly buildx_version_receipt="$last_command_receipt"
if ! run_docker host.client-plugin-inventory host 30 8388608 \
  info --format '{{json .ClientInfo.Plugins}}'; then
  echo "linux-docker: bounded Docker client-plugin inventory query failed" >&2
  exit 2
fi
readonly plugin_inventory_stdout="$last_command_stdout"
readonly plugin_inventory_stderr="$last_command_stderr"
readonly plugin_inventory_receipt="$last_command_receipt"
if ! run_docker \
  host.base-image-index host 120 67108864 \
  buildx imagetools inspect --raw "$base_image"; then
  echo "linux-docker: bounded base-image index query failed" >&2
  exit 2
fi
readonly base_image_index_spool="$last_command_stdout"
"$evidence" base-index \
  "$base_image" \
  "$base_image_index_spool" \
  "$output_root/host-base-image-index.json"

write_cell_result() {
  local output="$1"
  local status="$2"
  local exit_code="$3"
  local platform="$4"
  local reason="$5"
  printf '{"schema_version":1,"status":"%s","exit_code":%s,"platform":"%s","reason":"%s"}\n' \
    "$status" "$exit_code" "$platform" "$reason" \
    | "$evidence" write-json "$output/host-result.json"
}

preflight_resource_absent() {
  local kind="$1"
  local name="$2"
  local label="$3"
  local scope="$4"
  local inspect_status=125
  case "$kind" in
    builder)
      if run_docker "$label.inspect" "$scope" 60 8388608 \
        buildx inspect "$name"; then
        inspect_status=0
      else
        inspect_status=$?
      fi
      ;;
    container)
      if run_docker "$label.inspect" "$scope" 60 8388608 \
        container inspect "$name"; then
        inspect_status=0
      else
        inspect_status=$?
      fi
      ;;
    image)
      if run_docker "$label.inspect" "$scope" 60 8388608 \
        image inspect "$name"; then
        inspect_status=0
      else
        inspect_status=$?
      fi
      ;;
    *) return 2 ;;
  esac
  if [[ $inspect_status -eq 0 ]]; then
    echo "linux-docker: planned $kind already exists; refusing to reuse it: $name" >&2
    return 1
  fi
  if [[ $inspect_status -ne 1 ]]; then
    echo "linux-docker: planned $kind absence could not be proven: $name" >&2
    return 2
  fi
  case "$kind" in
    builder)
      run_docker "$label.list" "$scope" 60 8388608 \
        buildx ls --format '{{.Name}}' || return 2
      ;;
    container)
      run_docker "$label.list" "$scope" 60 8388608 \
        container ls --all --format '{{.Names}}' || return 2
      ;;
    image)
      run_docker "$label.list" "$scope" 60 8388608 \
        image ls --all --format '{{.Repository}}:{{.Tag}}' || return 2
      ;;
  esac
  local list_log="$last_command_stdout"
  while IFS= read -r discovered; do
    if [[ "$kind" == builder ]]; then
      discovered="${discovered%\*}"
    fi
    if [[ "$discovered" == "$name" ]]; then
      echo "linux-docker: planned $kind appears in the exact listing: $name" >&2
      return 1
    fi
  done <"$list_log"
}

overall=0
cell_outputs=()
cell_platforms=()
cell_test_statuses=()
cell_copy_statuses=()
cell_tree_statuses=()
cell_cleanup_statuses=()
cell_image_tags=()
cell_image_ids=()
for platform in "${platforms[@]}"; do
  arch="${platform#linux/}"
  output="$output_root/$arch"
  "$evidence" prepare-output "$output"
  suffix="$run_id-${expected_source:0:12}"
  builder="pmux-linux-builder-$arch-$suffix"
  builder_node_name="buildx_buildkit_${builder}0"
  image="pmux-linux-deterministic:$arch-$suffix"
  container="pmux-linux-$arch-$suffix"

  if ! preflight_resource_absent \
    builder "$builder" "$arch.preflight-builder" "$platform" \
    || ! preflight_resource_absent \
      container "$builder_node_name" "$arch.preflight-builder-node" "$platform" \
    || ! preflight_resource_absent \
      container "$container" "$arch.preflight-container" "$platform" \
    || ! preflight_resource_absent \
      image "$image" "$arch.preflight-image" "$platform"; then
    write_cell_result "$output" fail 1 "$platform" planned_resource_not_absent
    overall=1
    continue
  fi

  record_resource builder "$builder" pending planned
  record_resource container "$container" pending planned
  record_resource image "$image" pending planned
  active_builder_name="$builder"
  active_builder_identity=""
  active_builder_created=false
  active_builder_attempted=true
  active_scope="$platform"
  active_label_prefix="$arch"
  builder_create_status=125
  if run_docker \
    "$arch.builder-create" "$platform" 120 8388608 \
    buildx create --name "$builder" --driver docker-container; then
    builder_create_status=0
  else
    builder_create_status=$?
  fi
  builder_receipt="$(<"$last_command_stdout")"
  if [[ $builder_create_status -ne 0 || "$builder_receipt" != "$builder" ]]; then
    write_cell_result "$output" fail 1 "$platform" builder_create_failed
    overall=1
    cleanup_builder || overall=1
    continue
  fi
  inspect_status=125
  if run_docker \
    "$arch.builder-bootstrap" "$platform" 300 16777216 \
    buildx inspect --bootstrap "$builder"; then
    inspect_status=0
  else
    inspect_status=$?
  fi
  builder_inspect_spool="$last_command_stdout"
  node_status=125
  if run_docker \
    "$arch.builder-node-identity" "$platform" 60 8388608 \
    container inspect --format '{{.Id}}' "$builder_node_name"; then
    node_status=0
  else
    node_status=$?
  fi
  if [[ $node_status -eq 0 ]]; then
    active_builder_identity="$(<"$last_command_stdout")"
  fi
  if [[ "$active_builder_identity" =~ ^[0-9a-f]{64}$ ]]; then
    active_builder_created=true
    active_builder_attempted=false
    created_builder_count=$((created_builder_count + 1))
    record_resource builder "$builder" "$active_builder_identity" created
    record_resource builder "$builder" "$active_builder_identity" bound
  fi
  if [[ $inspect_status -ne 0 ]]; then
    write_cell_result "$output" fail "$inspect_status" "$platform" builder_bootstrap_failed
    overall=1
    cleanup_builder || overall=1
    continue
  fi
  if [[ "$active_builder_created" != true ]]; then
    write_cell_result "$output" fail 1 "$platform" builder_identity_capture_failed
    overall=1
    cleanup_builder || overall=1
    continue
  fi
  if ! "$evidence" platform-report \
    "$platform" "$builder_inspect_spool" "$output/builder-platforms.json"; then
    write_cell_result "$output" fail 1 "$platform" builder_platform_unsupported
    overall=1
    cleanup_builder || overall=1
    continue
  fi

  echo "linux-docker: building frozen $platform source as $image"
  iid_file="$output/build-image.iid"
  if [[ -e "$iid_file" || -L "$iid_file" ]]; then
    write_cell_result "$output" fail 1 "$platform" iid_destination_not_empty
    overall=1
    cleanup_builder || overall=1
    continue
  fi
  active_image_tag="$image"
  active_image_id=""
  active_image_created=false
  active_image_attempted=true
  build_status=125
  if run_docker \
    "$arch.image-build" "$platform" 7200 536870912 \
    buildx build \
      --builder "$builder" \
      --platform "$platform" \
      --build-arg "PMUX_EXPECTED_SOURCE_SHA256=$expected_source" \
      --build-arg "PMUX_RUST_BASE=$base_image" \
      --iidfile "$iid_file" \
      --load \
      --file "$workspace/tools/linux-docker/Dockerfile" \
      --tag "$image" \
      "$workspace"; then
    build_status=0
  else
    build_status=$?
  fi
  image_receipt=""
  image_discovered=""
  if [[ $build_status -eq 0 ]]; then
    if image_receipt="$("$evidence" image-iid "$iid_file")"; then
      iid_status=0
    else
      iid_status=$?
    fi
    if [[ $iid_status -eq 0 ]]; then
      if run_docker \
        "$arch.image-created-identity" "$platform" 60 8388608 \
        image inspect --format '{{.Id}}' "$image"; then
        image_inspect_status=0
        image_discovered="$(<"$last_command_stdout")"
      else
        image_inspect_status=$?
      fi
    else
      image_inspect_status=1
    fi
  else
    iid_status=1
    image_inspect_status=1
  fi
  if [[ $build_status -eq 0 && $iid_status -eq 0 && $image_inspect_status -eq 0 \
    && "$image_receipt" == "$image_discovered" ]]; then
    active_image_id="$image_receipt"
    active_image_created=true
    active_image_attempted=false
    loaded_image_count=$((loaded_image_count + 1))
    record_resource image "$image" "$active_image_id" created
  fi
  if ! cleanup_builder; then
    write_cell_result "$output" fail 1 "$platform" builder_cleanup_failed
    overall=1
    cleanup_image || overall=1
    break
  fi
  if [[ $build_status -ne 0 || "$active_image_created" != true ]]; then
    reported_build_status=$build_status
    if [[ $reported_build_status -eq 0 ]]; then
      reported_build_status=1
    fi
    write_cell_result \
      "$output" fail "$reported_build_status" "$platform" build_or_load_failed
    overall=1
    cleanup_image || overall=1
    continue
  fi

  image_identity_status=0
  if run_docker \
    "$arch.image-source-label" "$platform" 60 8388608 \
    image inspect --format '{{index .Config.Labels "io.pmux.source-sha256"}}' \
    "$active_image_id"; then
    image_source="$(<"$last_command_stdout")"
  else
    image_source=""
    image_identity_status=1
  fi
  if run_docker \
    "$arch.image-architecture" "$platform" 60 8388608 \
    image inspect --format '{{.Architecture}}' "$active_image_id"; then
    image_arch="$(<"$last_command_stdout")"
  else
    image_arch=""
    image_identity_status=1
  fi
  if run_docker \
    "$arch.image-base-label" "$platform" 60 8388608 \
    image inspect --format '{{index .Config.Labels "io.pmux.base-image"}}' \
    "$active_image_id"; then
    image_base="$(<"$last_command_stdout")"
  else
    image_base=""
    image_identity_status=1
  fi
  if [[ $image_identity_status -ne 0 \
    || "$image_source" != "$expected_source" || "$image_arch" != "$arch" \
    || "$image_base" != "$base_image" ]]; then
    write_cell_result "$output" fail 1 "$platform" image_identity_mismatch
    overall=1
    cleanup_image || overall=1
    continue
  fi
  printf '{"schema_version":1,"tag":"%s","id":"%s","source_sha256":"%s","architecture":"%s","base_image":"%s"}\n' \
    "$image" "$active_image_id" "$image_source" "$image_arch" "$image_base" \
    | "$evidence" write-json "$output/host-image-identity.json"

  active_container_name="$container"
  active_container_id=""
  active_container_created=false
  active_container_attempted=true
  create_status=125
  if run_docker \
    "$arch.container-create" "$platform" 120 8388608 \
    create \
      --name "$container" \
      --platform "$platform" \
      --init \
      --network none \
      --pids-limit 2048 \
      --cap-drop ALL \
      --cap-add CHOWN \
      --cap-add DAC_READ_SEARCH \
      --cap-add KILL \
      --cap-add SETGID \
      --cap-add SETUID \
      --security-opt no-new-privileges \
      --tmpfs /tmp:rw,exec,nosuid,nodev,mode=1777 \
      "$active_image_id"; then
    create_status=0
    active_container_id="$(<"$last_command_stdout")"
  else
    create_status=$?
  fi
  if [[ $create_status -ne 0 || ! "$active_container_id" =~ ^[0-9a-f]{64}$ ]]; then
    write_cell_result "$output" fail 1 "$platform" container_create_failed
    overall=1
    cleanup_container || overall=1
    cleanup_image || overall=1
    continue
  fi
  active_container_created=true
  active_container_attempted=false
  created_container_count=$((created_container_count + 1))
  record_resource container "$container" "$active_container_id" created

  test_status=125
  if run_docker \
    "$arch.container-start-attach" "$platform" 7200 536870912 \
    start --attach "$active_container_id"; then
    test_status=0
  else
    test_status=$?
  fi
  copy_stage="$output/container-artifacts"
  "$evidence" prepare-output "$copy_stage"
  copy_status=125
  if run_docker \
    "$arch.container-artifact-copy" "$platform" 600 16777216 \
    cp "$active_container_id:/artifacts/." "$copy_stage/"; then
    copy_status=0
  else
    copy_status=$?
  fi
  tree_status=1
  if [[ $copy_status -eq 0 ]]; then
    set +e
    "$evidence" secure-tree "$copy_stage"
    secure_copy_status=$?
    if [[ $secure_copy_status -eq 0 \
      && -f "$copy_stage/container-artifact-tree-final.json" ]]; then
      "$evidence" tree-verify \
        "$copy_stage" "$copy_stage/container-artifact-tree-final.json" \
        --output "$output/host-container-artifact-tree-binding.json"
      tree_status=$?
    fi
    set -e
  fi
  container_cleanup_status=0
  cleanup_container || container_cleanup_status=1

  cell_status=0
  if [[ $test_status -ne 0 || $copy_status -ne 0 \
    || $tree_status -ne 0 || $container_cleanup_status -ne 0 ]]; then
    cell_status=1
    overall=1
  fi
  cell_outputs+=("$output")
  cell_platforms+=("$platform")
  cell_test_statuses+=("$test_status")
  cell_copy_statuses+=("$copy_status")
  cell_tree_statuses+=("$tree_status")
  cell_cleanup_statuses+=("$container_cleanup_status")
  cell_image_tags+=("$image")
  cell_image_ids+=("$active_image_id")
  if ! cleanup_image; then
    overall=1
    break
  fi
done

"$source_digest" "$workspace" \
  --expected "$expected_source" \
  --json \
  --output "$output_root/host-source-after.json"
set +e
"$evidence" source-stability \
  "$output_root/host-source-before.json" \
  "$output_root/host-source-after.json" \
  "$expected_source" \
  "$output_root/host-source-stability.json"
source_stability_status=$?
set -e
if [[ $source_stability_status -ne 0 ]]; then
  overall=1
fi
set +e
"$source_digest" "$workspace" --revision-capture \
  --output "$output_root/host-revision-capture-after.json"
revision_capture_status=$?
if [[ $revision_capture_status -eq 0 ]]; then
  "$evidence" revision-stability \
    "$output_root/host-revision-capture-before.json" \
    "$output_root/host-revision-capture-after.json" \
    "$output_root/host-revision-stability.json"
  revision_stability_status=$?
else
  revision_stability_status=1
fi
set -e
if [[ $revision_stability_status -ne 0 ]]; then
  overall=1
fi

# A cell cannot be bound until the after-capture proves that the host revision
# remained identical across the entire multi-platform run.  Finalize every
# otherwise-complete cell only after that run-level fact exists.
for ((cell_index = 0; cell_index < ${#cell_outputs[@]}; cell_index++)); do
  output="${cell_outputs[$cell_index]}"
  platform="${cell_platforms[$cell_index]}"
  copy_stage="$output/container-artifacts"
  binding_status=1
  if [[ $revision_stability_status -eq 0 \
    && "${cell_tree_statuses[$cell_index]}" -eq 0 \
    && -f "$copy_stage/system.json" \
    && -f "$copy_stage/image-release-binaries.json" \
    && -f "$copy_stage/release-binaries-before.json" \
    && -f "$copy_stage/release-binaries-after.json" \
    && -f "$copy_stage/repro-release-staged.json" \
    && -f "$copy_stage/repro-release-comparison.json" \
    && -f "$copy_stage/uds-binary-binding.json" \
    && -f "$copy_stage/platform-gate-a-manifest.json" \
    && -f "$copy_stage/result.json" ]]; then
    set +e
    "$evidence" cell-binding \
      "$output_root/host-source-before.json" \
      "$output_root/host-revision-capture-before.json" \
      "$output_root/host-revision-capture-after.json" \
      "$output_root/host-revision-stability.json" \
      "$copy_stage/system.json" \
      "$copy_stage/image-release-binaries.json" \
      "$copy_stage/release-binaries-before.json" \
      "$copy_stage/release-binaries-after.json" \
      "$copy_stage/repro-release-staged.json" \
      "$copy_stage/repro-release-comparison.json" \
      "$copy_stage/uds-binary-binding.json" \
      "$copy_stage/result.json" \
      "$copy_stage/platform-gate-a-manifest.json" \
      "$expected_source" \
      "$platform" \
      "$base_image" \
      "$output/host-container-source-binding.json"
    binding_status=$?
    set -e
  else
    printf '{"schema_version":1,"verified":false,"reason":"required_container_or_revision_evidence_missing"}\n' \
      | "$evidence" write-json "$output/host-container-source-binding.json"
  fi
  cell_status=0
  if [[ "${cell_test_statuses[$cell_index]}" -ne 0 \
    || "${cell_copy_statuses[$cell_index]}" -ne 0 \
    || "${cell_tree_statuses[$cell_index]}" -ne 0 \
    || "${cell_cleanup_statuses[$cell_index]}" -ne 0 \
    || $binding_status -ne 0 ]]; then
    cell_status=1
    overall=1
  fi
  printf '{"schema_version":1,"status":"%s","exit_code":%s,"container_exit_code":%s,"artifact_copy_exit_code":%s,"container_artifact_tree_verified":%s,"container_cleanup_verified":%s,"source_binary_binding_verified":%s,"platform":"%s","image_tag":"%s","image_id":"%s"}\n' \
    "$([[ $cell_status -eq 0 ]] && echo pass || echo fail)" \
    "$cell_status" "${cell_test_statuses[$cell_index]}" \
    "${cell_copy_statuses[$cell_index]}" \
    "$([[ "${cell_tree_statuses[$cell_index]}" -eq 0 ]] && echo true || echo false)" \
    "$([[ "${cell_cleanup_statuses[$cell_index]}" -eq 0 ]] && echo true || echo false)" \
    "$([[ $binding_status -eq 0 ]] && echo true || echo false)" \
    "$platform" "${cell_image_tags[$cell_index]}" \
    "${cell_image_ids[$cell_index]}" \
    | "$evidence" write-json "$output/host-result.json"
done

cleanup_container || overall=1
cleanup_builder || overall=1
cleanup_image || overall=1
if [[ $created_builder_count -ne $removed_builder_count \
  || $created_container_count -ne $removed_container_count \
  || $loaded_image_count -ne $removed_image_count \
  || $unconfirmed_builder_count -ne 0 \
  || $unconfirmed_container_count -ne 0 \
  || $unconfirmed_image_count -ne 0 ]]; then
  overall=1
fi
write_cleanup_report || overall=1
write_command_report || overall=1
set +e
"$evidence" docker-transport "$docker_socket_path" "$docker_transport_after"
transport_after_status=$?
if [[ $transport_after_status -eq 0 ]]; then
  "$evidence" docker-transport-stability \
    "$docker_transport_before" "$docker_transport_after" \
    "$output_root/host-docker-transport-stability.json"
  transport_stability_status=$?
else
  transport_stability_status=1
fi
set -e
if [[ $transport_stability_status -ne 0 ]]; then
  overall=1
fi
set +e
"$evidence" docker-control-plane \
  "$workspace" \
  "$docker_version_receipt" "$docker_version_stdout" "$docker_version_stderr" \
  "$buildx_version_receipt" "$buildx_version_stdout" "$buildx_version_stderr" \
  "$plugin_inventory_receipt" "$plugin_inventory_stdout" \
  "$plugin_inventory_stderr" "$docker_transport_before" \
  "$output_root/host-docker-buildx-control-plane.json"
control_plane_status=$?
set -e
if [[ $control_plane_status -ne 0 ]]; then
  overall=1
fi
"$evidence" secure-tree "$output_root"
"$evidence" tree-manifest \
  "$output_root" "$output_root/host-evidence-tree-final.json"
"$evidence" secure-tree "$output_root"

echo "linux-docker: evidence written to $output_root"
exit "$overall"
