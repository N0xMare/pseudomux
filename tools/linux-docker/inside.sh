#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly workspace=/workspace
readonly artifacts=/artifacts
readonly evidence="$workspace/tools/linux-docker/evidence.py"
readonly frozen_source="${PMUX_FROZEN_SOURCE_SHA256:?PMUX_FROZEN_SOURCE_SHA256 is required}"
readonly container_platform="${PMUX_CONTAINER_PLATFORM:?PMUX_CONTAINER_PLATFORM is required}"
readonly base_image="${PMUX_BASE_IMAGE_REF:?PMUX_BASE_IMAGE_REF is required}"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "linux-docker: this suite requires a Linux kernel" >&2
  exit 2
fi
if [[ "$(id -u)" != "0" ]]; then
  echo "linux-docker: the container supervisor must start as root" >&2
  exit 2
fi
if [[ ! "$frozen_source" =~ ^[0-9a-f]{64}$ ]]; then
  echo "linux-docker: frozen source identity is malformed" >&2
  exit 2
fi
"$evidence" base-image "$base_image" >/dev/null
case "$container_platform" in
  linux/arm64|linux/amd64) ;;
  *)
    echo "linux-docker: container platform is outside the declared matrix" >&2
    exit 2
    ;;
esac
if command -v claude >/dev/null 2>&1; then
  echo "linux-docker: refusing to run with a Claude executable in PATH" >&2
  exit 2
fi

for private_directory in "$artifacts" /var/tmp/pmux-linux-suite; do
  if [[ ! -d "$private_directory" || -L "$private_directory" \
    || "$(stat -c %u "$private_directory")" != 10001 \
    || "$(stat -c %a "$private_directory")" != 700 ]]; then
    echo "linux-docker: prebuilt private directory identity is invalid: $private_directory" >&2
    exit 2
  fi
done
if find "$artifacts" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
  echo "linux-docker: artifact directory must start empty" >&2
  exit 2
fi

readonly clean_path=/opt/pmux-python/bin:/opt/pmux-cargo-fuzz/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin
readonly release_dir=/opt/pmux-candidate/bin
readonly initial_binaries="$artifacts/image-release-binaries.json"

if [[ ! -d "$release_dir" || -L "$release_dir" \
  || "$(stat -c %u "$release_dir")" != 0 \
  || "$(stat -c %a "$release_dir")" != 555 ]]; then
  echo "linux-docker: frozen image candidate identity is invalid" >&2
  exit 2
fi

# Bind the source and every release executable before the root-only probe can
# execute pmuxd or pmux.
runuser -u pmux -- env -i \
  HOME=/home/pmux \
  USER=pmux \
  LOGNAME=pmux \
  PATH="$clean_path" \
  "$workspace/tools/linux-docker/source_digest.py" "$workspace" \
    --expected "$frozen_source" \
    --json \
    --output "$artifacts/container-source-before.json"
runuser -u pmux -- env -i \
  HOME=/home/pmux \
  USER=pmux \
  LOGNAME=pmux \
  PATH="$clean_path" \
  "$evidence" binary-capture "$release_dir" "$initial_binaries" \
    --expected-owner-uid 0

# Root exists only for this cross-UID denial proof.  The report is first
# published in a root-owned private directory, then transferred once to the
# unprivileged artifact owner.
root_evidence="$(mktemp -d /var/tmp/pmux-root-evidence.XXXXXXXX)"
readonly root_evidence
chmod 0700 "$root_evidence"
root_report="$root_evidence/uds-permissions.json"
readonly root_report
root_stdout="$root_evidence/uds-probe.stdout"
root_stderr="$root_evidence/uds-probe.stderr"
root_receipt="$root_evidence/uds-probe.receipt.json"
readonly root_stdout root_stderr root_receipt
probe_status=125
if "$workspace/tools/linux-docker/bounded_runner.py" \
  --cwd "$workspace" \
  --timeout-seconds 90 \
  --drain-timeout-seconds 10 \
  --maximum-output-bytes 16777216 \
  --stdout "$root_stdout" \
  --stderr "$root_stderr" \
  --receipt "$root_receipt" \
  --description 'root-supervised cross-UID UDS permission proof' \
  --env 'HOME=/nonexistent-pmux-root-home' \
  --env 'LANG=C' \
  --env 'LC_ALL=C' \
  --env 'PATH=/usr/sbin:/usr/bin:/bin' \
  --env 'PYTHONDONTWRITEBYTECODE=1' \
  --env 'TZ=UTC' \
  -- /usr/bin/python3 "$workspace/tools/linux-docker/permissions_probe.py" \
    "$root_report" "$initial_binaries" >/dev/null; then
  probe_status=0
else
  probe_status=$?
fi
transfer_status=0
for transfer in \
  "$root_report:$artifacts/uds-permissions.json:67108864" \
  "$root_stdout:$artifacts/uds-probe.stdout:16777216" \
  "$root_stderr:$artifacts/uds-probe.stderr:16777216" \
  "$root_receipt:$artifacts/uds-probe.receipt.json:4194304"; do
  source_path="${transfer%%:*}"
  remainder="${transfer#*:}"
  destination_path="${remainder%%:*}"
  byte_bound="${remainder##*:}"
  if [[ ! -f "$source_path" || -L "$source_path" ]] \
    || ! "$evidence" transfer-private \
      "$source_path" "$destination_path" 10001 10001 "$byte_bound" \
      >/dev/null; then
    transfer_status=1
  fi
done
if ! rmdir "$root_evidence"; then
  transfer_status=1
fi
if [[ $transfer_status -ne 0 ]]; then
  probe_status=1
fi
if [[ $probe_status -ne 0 ]]; then
  printf '%s\n' \
    '{"schema_version":1,"status":"fail","failure_count":1,"gate_count":1,"gates":[{"name":"cross_uid_uds","outcome":"FAIL(1)","elapsed_seconds":0,"command_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}]}' \
    | runuser -u pmux -- env -i HOME=/home/pmux PATH="$clean_path" \
        "$evidence" write-json "$artifacts/result.json"
  echo "linux-docker: cross-UID UDS probe failed" >&2
  exit 1
fi

# All compilation, PTY, fake-child, package, and conformance gates run as uid
# 10001 with no inherited environment and no effective capabilities.
exec runuser -u pmux -- env -i \
  HOME=/home/pmux \
  USER=pmux \
  LOGNAME=pmux \
  LANG=C.UTF-8 \
  LC_ALL=C.UTF-8 \
  TZ=UTC \
  PATH="$clean_path" \
  PYTHONDONTWRITEBYTECODE=1 \
  RUSTUP_HOME=/usr/local/rustup \
  CARGO_HOME=/usr/local/cargo \
  RUSTUP_TOOLCHAIN=1.88.0 \
  CARGO_NET_OFFLINE=true \
  CARGO_TERM_COLOR=never \
  RUST_BACKTRACE=1 \
  npm_config_audit=false \
  npm_config_fund=false \
  npm_config_offline=true \
  PMUX_LINUX_ARTIFACTS="$artifacts" \
  PMUX_LINUX_CANDIDATE_DIR="$release_dir" \
  PMUX_FROZEN_SOURCE_SHA256="$frozen_source" \
  PMUX_CONTAINER_PLATFORM="$container_platform" \
  PMUX_BASE_IMAGE_REF="$base_image" \
  "$workspace/tools/linux-docker/suite.sh"
