#!/usr/bin/env bash
set -uo pipefail
umask 077

verify_typescript_stage_identity() {
  if [[ $# -ne 5 ]]; then
    echo "linux-docker: TypeScript stage identity check requires five arguments" >&2
    return 2
  fi
  local expected=$1
  local node_binary=$2
  local verifier=$3
  local stage=$4
  local outside_root=$5
  local observed
  if [[ ! "$expected" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: captured TypeScript stage identity is malformed" >&2
    return 1
  fi
  observed="$(
    "$node_binary" "$verifier" verify "$stage" --outside-root "$outside_root"
  )" || return 1
  if [[ ! "$observed" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: TypeScript stage verifier returned a malformed identity" >&2
    return 1
  fi
  printf '%s\n' "$observed"
  if [[ "$observed" != "$expected" ]]; then
    echo "linux-docker: TypeScript stage changed after its Gate A freeze" >&2
    return 1
  fi
}

cleanup_validation_outputs() {
  if [[ $# -ne 1 || "$1" != /var/tmp/pmux-linux-suite/validation ]]; then
    echo "linux-docker: validation cleanup target is not exact" >&2
    return 2
  fi
  find "$1" -depth -delete
  [[ ! -e "$1" ]]
}

if [[ "${1:-}" == "--verify-typescript-stage-identity" ]]; then
  shift
  verify_typescript_stage_identity "$@"
  exit $?
fi
if [[ "${1:-}" == "--cleanup-validation-outputs" ]]; then
  shift
  cleanup_validation_outputs "$@"
  exit $?
fi
if [[ $# -ne 0 ]]; then
  echo "linux-docker: unknown suite argument" >&2
  exit 2
fi

readonly workspace=/workspace
readonly artifacts="${PMUX_LINUX_ARTIFACTS:?PMUX_LINUX_ARTIFACTS is required}"
readonly frozen_source="${PMUX_FROZEN_SOURCE_SHA256:?PMUX_FROZEN_SOURCE_SHA256 is required}"
readonly container_platform="${PMUX_CONTAINER_PLATFORM:?PMUX_CONTAINER_PLATFORM is required}"
readonly evidence="$workspace/tools/linux-docker/evidence.py"
readonly bounded_runner="$workspace/tools/linux-docker/bounded_runner.py"
readonly suite_script="$workspace/tools/linux-docker/suite.sh"
readonly summary="$artifacts/gates.tsv"
readonly gate_evidence_ledger="$artifacts/gate-evidence-ledger.ndjson"
readonly declared_gate_manifest="$workspace/tools/linux-docker/gate-a-manifest.json"
readonly platform_gate_manifest="$artifacts/platform-gate-a-manifest.json"
readonly release_dir="${PMUX_LINUX_CANDIDATE_DIR:?PMUX_LINUX_CANDIDATE_DIR is required}"
readonly initial_binaries="$artifacts/image-release-binaries.json"
readonly candidate_before="$artifacts/release-binaries-before.json"
readonly candidate_after="$artifacts/release-binaries-after.json"
readonly validation_root=/var/tmp/pmux-linux-suite/validation
readonly cargo_target_root="$validation_root/cargo-target"
readonly workspace_target="$cargo_target_root/workspace"
readonly vendor_client_target="$cargo_target_root/vendor-rmux-client"
readonly vendor_server_target="$cargo_target_root/vendor-rmux-server"
readonly repro_release_target="$cargo_target_root/repro-release"
readonly repro_bin="$validation_root/repro-bin"
readonly typescript_dist="$validation_root/typescript-dist"
readonly fuzz_target="$validation_root/fuzz"
readonly repro_comparison="$artifacts/repro-release-comparison.json"
readonly repro_stage_manifest="$artifacts/repro-release-staged.json"
readonly nightly_toolchain=nightly-2026-03-26
nightly_cargo="$(rustup which --toolchain "$nightly_toolchain" cargo)" || {
  echo "linux-docker: exact nightly cargo is unavailable" >&2
  exit 2
}
nightly_rustc="$(rustup which --toolchain "$nightly_toolchain" rustc)" || {
  echo "linux-docker: exact nightly rustc is unavailable" >&2
  exit 2
}
nightly_bin="$(dirname "$nightly_cargo")"
readonly nightly_cargo nightly_rustc nightly_bin
if [[ "$nightly_cargo" != "$nightly_bin/cargo" || "$nightly_rustc" != "$nightly_bin/rustc" ]]; then
  echo "linux-docker: pinned nightly tools do not share one exact bin directory" >&2
  exit 2
fi
failures=0
LAST_GATE_STATUS=0
gate_evidence_ordinal=0
gate_evidence_anchor=START

cd "$workspace" || exit 2
: >"$summary"
chmod 0600 "$summary"
if [[ "$release_dir" != /opt/pmux-candidate/bin ]]; then
  echo "linux-docker: candidate directory differs from the frozen image plane" >&2
  exit 2
fi
if find /var/tmp/pmux-linux-suite -mindepth 1 -print -quit | grep -q .; then
  echo "linux-docker: validation plane must start empty" >&2
  exit 2
fi
install -d -m 0700 \
  "$validation_root" \
  "$cargo_target_root" \
  "$workspace_target" \
  "$vendor_client_target" \
  "$vendor_server_target" \
  "$repro_release_target" \
  "$repro_bin" \
  "$typescript_dist" \
  "$fuzz_target"
export CARGO_TARGET_DIR="$workspace_target"

if [[ "$(id -u)" == "0" ]]; then
  echo "linux-docker: product tests must not run as root" >&2
  exit 2
fi
if [[ ! "$frozen_source" =~ ^[0-9a-f]{64}$ ]]; then
  echo "linux-docker: frozen source identity is malformed" >&2
  exit 2
fi
effective_caps="$(awk '/^CapEff:/ {print $2}' /proc/self/status)"
if [[ "$effective_caps" != "0000000000000000" ]]; then
  echo "linux-docker: product tests retained effective capabilities: $effective_caps" >&2
  exit 2
fi

"$evidence" gate-manifest \
  "$declared_gate_manifest" "$container_platform" "$platform_gate_manifest"

gate_environment_arguments=()
for variable in \
  HOME USER LOGNAME LANG LC_ALL TZ PATH RUSTUP_HOME CARGO_HOME \
  RUSTUP_TOOLCHAIN CARGO_NET_OFFLINE CARGO_TERM_COLOR RUST_BACKTRACE \
  PYTHONDONTWRITEBYTECODE \
  npm_config_audit npm_config_fund npm_config_offline \
  PMUX_LINUX_ARTIFACTS PMUX_LINUX_CANDIDATE_DIR \
  PMUX_FROZEN_SOURCE_SHA256 PMUX_CONTAINER_PLATFORM PMUX_BASE_IMAGE_REF \
  CARGO_TARGET_DIR; do
  if [[ ! -v "$variable" ]]; then
    echo "linux-docker: bounded gate environment is missing $variable" >&2
    exit 2
  fi
  gate_environment_arguments+=(--env "$variable=${!variable}")
done
readonly -a gate_environment_arguments

run_gate() {
  local name="$1"
  shift
  local started ended elapsed status receipt_sha outcome next_anchor
  local stdout_path stderr_path receipt_path
  if [[ ! "$name" =~ ^[a-z0-9_]+$ || $# -eq 0 ]]; then
    echo "linux-docker: bounded gate invocation is malformed" >&2
    return 2
  fi
  stdout_path="$artifacts/$name.log"
  stderr_path="$artifacts/$name.stderr"
  receipt_path="$artifacts/$name.receipt.json"
  started="$(date +%s)"
  echo "linux-docker: BEGIN $name"
  if receipt_sha="$(
    python3 "$bounded_runner" \
      --cwd "$workspace" \
      --timeout-seconds 3600 \
      --drain-timeout-seconds 30 \
      --maximum-output-bytes 8388608 \
      --stdout "$stdout_path" \
      --stderr "$stderr_path" \
      --receipt "$receipt_path" \
      --description "Linux deterministic gate $name" \
      "${gate_environment_arguments[@]}" \
      -- "$@"
  )"; then
    status=0
  else
    status=$?
  fi
  if [[ ! "$receipt_sha" =~ ^[0-9a-f]{64}$ || ! -f "$receipt_path" ]]; then
    receipt_sha=0000000000000000000000000000000000000000000000000000000000000000
    status=125
  fi
  ended="$(date +%s)"
  elapsed=$((ended - started))
  LAST_GATE_STATUS=$status
  if [[ $status -eq 0 ]]; then
    outcome=PASS
    echo "linux-docker: PASS $name (${elapsed}s)"
  else
    outcome="FAIL($status)"
    echo "linux-docker: FAIL $name status=$status (${elapsed}s)" >&2
    failures=$((failures + 1))
  fi
  if [[ "$receipt_sha" == 0000000000000000000000000000000000000000000000000000000000000000 ]]; then
    echo "linux-docker: gate has no complete bounded receipt: $name" >&2
    exit 125
  fi
  next_anchor="$(
    "$evidence" append-gate \
      "$gate_evidence_ledger" "$receipt_path" "$name" "$outcome" "$elapsed" \
      --expected-ordinal "$((gate_evidence_ordinal + 1))" \
      --expected-prior-sha256 "$gate_evidence_anchor"
  )" || {
    echo "linux-docker: gate evidence could not be externally chained: $name" >&2
    exit 125
  }
  if [[ ! "$next_anchor" =~ ^[0-9a-f]{64}$ ]]; then
    echo "linux-docker: gate evidence returned an invalid external anchor" >&2
    exit 125
  fi
  gate_evidence_ordinal=$((gate_evidence_ordinal + 1))
  gate_evidence_anchor=$next_anchor
  printf '%s\t%s\t%s\t%s\n' "$name" "$outcome" "$elapsed" "$receipt_sha" >>"$summary"
}

skip_gate() {
  local name="$1"
  local skip_path="$artifacts/$name.skip.json"
  local skip_sha next_anchor
  skip_sha="$("$evidence" publish-gate-skip "$artifacts" "$name")" || exit 125
  next_anchor="$(
    "$evidence" append-gate-skip \
      "$gate_evidence_ledger" "$skip_path" "$name" \
      --expected-ordinal "$((gate_evidence_ordinal + 1))" \
      --expected-prior-sha256 "$gate_evidence_anchor"
  )" || exit 125
  [[ "$skip_sha" =~ ^[0-9a-f]{64}$ && "$next_anchor" =~ ^[0-9a-f]{64}$ ]] \
    || exit 125
  gate_evidence_ordinal=$((gate_evidence_ordinal + 1))
  gate_evidence_anchor=$next_anchor
  printf '%s\tFAIL(SKIPPED_PREREQUISITE)\t0\t%s\n' \
    "$name" \
    "$skip_sha" \
    >>"$summary"
  failures=$((failures + 1))
  LAST_GATE_STATUS=1
}

# Identity and declared exclusions are evidence gates, not product oracles.
run_gate system_identity \
  "$evidence" system "$workspace" "$frozen_source" "$container_platform" \
  "$artifacts/system.json"
run_gate image_release_binary_identity \
  "$evidence" binary-verify "$initial_binaries" \
  --output "$artifacts/image-release-binaries-verified.json"
run_gate cross_uid_uds_report \
  "$evidence" uds-binding \
  "$artifacts/uds-permissions.json" "$initial_binaries" \
  "$artifacts/uds-probe.receipt.json" \
  "$artifacts/uds-probe.stdout" "$artifacts/uds-probe.stderr" \
  "$artifacts/uds-binary-binding.json"
printf '%s\n' \
  '{"schema_version":1,"claim":"docker_portability_only","named_exclusions":[{"id":"gate_b_real_claude_macos","reason":"external credentialed macOS promotion runs only after deterministic freeze"},{"id":"native_host_pty_timing","reason":"Docker namespaces and emulation do not calibrate native host PTY/process timing"},{"id":"native_linux_credentialed_claude","reason":"future external support milestone; Docker never contains Claude or credentials"}]}' \
  | "$evidence" write-json "$artifacts/platform-exclusions.json"

# Gate A/A: exact static, build, documentation, ordinary language, and package
# checks.  Every command is offline in this runtime.
run_gate rust_fmt cargo +1.88 fmt --all -- --check
run_gate rust_check \
  cargo +1.88 check --locked --workspace --all-targets --all-features
run_gate rust_clippy \
  cargo +1.88 clippy --locked --workspace --all-targets --all-features -- -D warnings
run_gate rustdoc \
  env RUSTDOCFLAGS=-Dwarnings \
  cargo +1.88 doc --locked --workspace --all-features --no-deps
run_gate rust_tests \
  cargo +1.88 test --locked --workspace --all-targets --all-features
run_gate rmux_vendor_standalone_fmt \
  cargo +1.88 fmt --manifest-path vendor/rmux-client/Cargo.toml --all -- --check
run_gate rmux_vendor_standalone_check \
  env CARGO_TARGET_DIR="$vendor_client_target" \
  cargo +1.88 check --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features
run_gate rmux_vendor_standalone_clippy \
  env CARGO_TARGET_DIR="$vendor_client_target" \
  cargo +1.88 clippy --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features \
  -- -D warnings
run_gate rmux_vendor_standalone_rustdoc \
  env CARGO_TARGET_DIR="$vendor_client_target" RUSTDOCFLAGS=-Dwarnings \
  cargo +1.88 doc --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-features --no-deps
run_gate rmux_vendor_standalone_tests \
  env CARGO_TARGET_DIR="$vendor_client_target" \
  cargo +1.88 test --offline --locked \
  --manifest-path vendor/rmux-client/Cargo.toml --all-targets --all-features \
  -- --test-threads=1
run_gate rmux_vendor_patch \
  cargo +1.88 test --locked -p pseudomux-rmux \
  --test vendor_patch -- --test-threads=1
run_gate rmux_attach_fragmentation \
  cargo +1.88 test --locked -p pseudomux-rmux \
  --test attach_fragmentation -- --test-threads=1
run_gate rmux_server_vendor_fmt \
  rustfmt +1.88 --edition 2021 --check \
  vendor/rmux-server/src/lib.rs vendor/rmux-server/build.rs
run_gate rmux_server_vendor_product_check \
  env CARGO_TARGET_DIR="$vendor_server_target" \
  cargo +1.88 check --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-targets --no-default-features
run_gate rmux_server_vendor_strict_clippy \
  env CARGO_TARGET_DIR="$vendor_server_target" \
  cargo +1.88 clippy --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-targets --all-features \
  -- -D warnings \
  -A clippy::collapsible-else-if -A clippy::uninlined-format-args
run_gate rmux_server_vendor_strict_rustdoc \
  env CARGO_TARGET_DIR="$vendor_server_target" RUSTDOCFLAGS=-Dwarnings \
  cargo +1.88 doc --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --all-features --no-deps
run_gate rmux_server_vendor_patch_regressions \
  env CARGO_TARGET_DIR="$vendor_server_target" \
  cargo +1.88 test --offline --locked \
  --manifest-path vendor/rmux-server/Cargo.toml --lib --no-default-features \
  pane_io::tests:: \
  -- --test-threads=1
run_gate rmux_server_vendor_patch \
  cargo +1.88 test --locked -p pseudomux-rmux \
  --test vendor_server_patch -- --test-threads=1

run_gate typescript_typecheck \
  node clients/typescript/node_modules/typescript/bin/tsc \
  -p clients/typescript/tsconfig.json --noEmit
run_gate typescript_stage_prepare \
  node clients/typescript/tests/dist-stage.mjs prepare \
  "$typescript_dist" --outside-root "$workspace"
run_gate typescript_external_build \
  node clients/typescript/node_modules/typescript/bin/tsc \
  -p clients/typescript/tsconfig.json --outDir "$typescript_dist"
run_gate typescript_stage_verify \
  node clients/typescript/tests/dist-stage.mjs verify \
  "$typescript_dist" --outside-root "$workspace"
typescript_stage_digest="$(<"$artifacts/typescript_stage_verify.log")"
readonly typescript_stage_digest
run_gate typescript_stage_identity_capture \
  bash "$suite_script" --verify-typescript-stage-identity \
  "$typescript_stage_digest" \
  node \
  "$workspace/clients/typescript/tests/dist-stage.mjs" \
  "$typescript_dist" \
  "$workspace"
run_gate typescript_tests \
  env PMUX_TYPESCRIPT_DIST_DIR="$typescript_dist" \
  node --test \
  clients/typescript/tests/client.test.mjs \
  clients/typescript/tests/dist-stage.test.mjs \
  clients/typescript/tests/golden-conformance.test.mjs
run_gate typescript_actual_daemon_syntax \
  node --check clients/typescript/tests/actual_daemon_e2e.mjs
run_gate python_client \
  env PYTHONDONTWRITEBYTECODE=1 \
  bash -c 'cd clients/python && exec python3 -m unittest discover -s tests -v'
run_gate python_ruff \
  python3 -m ruff check --no-cache \
  clients/python tools/package-smoke tools/phase0 tools/linux-docker \
  tools/gate-a-candidate
run_gate python_ruff_format \
  python3 -m ruff format --check --no-cache \
  clients/python tools/package-smoke tools/phase0 tools/linux-docker \
  tools/gate-a-candidate
run_gate typescript_package_artifact \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 tools/package-smoke/package_smoke.py typescript
run_gate python_package_artifact \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 tools/package-smoke/package_smoke.py python

# Gate A/B: bounded deterministic properties, models, and production fuzz
# targets.  The image prebuilt the exact nightly and cargo-fuzz dependencies.
run_gate transcript_properties \
  env PROPTEST_CASES=4096 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-claude --test transcript_properties
run_gate actor_model \
  env PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-service --test actor_model
run_gate client_protocol_properties \
  env PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pseudomux-client --lib protocol_properties
run_gate protocol_framing_properties \
  env PROPTEST_CASES=2048 PROPTEST_MAX_SHRINK_ITERS=10000 \
  cargo +1.88 test --locked -p pmuxd --bin pmuxd \
  handler::tests::arbitrary_admitted_payloads_have_bounded_decode_recovery_and_responses \
  -- --exact --test-threads=1
# shellcheck disable=SC2016  # the nested shell expands its explicit $1 argument
run_gate cargo_fuzz_version \
  bash -c 'test "$("$1" --version)" = "cargo-fuzz 0.13.2"' \
  bash /opt/pmux-cargo-fuzz/bin/cargo-fuzz
mkdir -p "$artifacts/fuzz-runs"
chmod 0700 "$artifacts/fuzz-runs"
run_gate production_fuzz \
  env \
  PMUX_FUZZ_RUNS=50000 \
  PMUX_CARGO_FUZZ_BIN=/opt/pmux-cargo-fuzz/bin/cargo-fuzz \
  PMUX_FUZZ_TARGET_DIR="$fuzz_target" \
  PMUX_FUZZ_EVIDENCE_ROOT="$artifacts/fuzz-runs" \
  PMUX_NIGHTLY_BIN_DIR="$nightly_bin" \
  PMUX_NIGHTLY_CARGO="$nightly_cargo" \
  PMUX_NIGHTLY_RUSTC="$nightly_rustc" \
  bash scripts/gate-a-fuzz.sh

# Gate A/C: serialized real-rmux/PTY and lifecycle faults.  These tests use
# controlled fake children and never a real Claude executable.
run_gate companion_debug_build \
  cargo +1.88 build --locked -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook
run_gate native_service \
  cargo +1.88 test --locked -p pseudomux-service \
  --test native_service -- --ignored --test-threads=1
run_gate private_runtime \
  cargo +1.88 test --locked -p pseudomux-service \
  --test private_runtime -- --ignored --test-threads=1
run_gate lifecycle_faults \
  cargo +1.88 test --locked -p pseudomux-service \
  --test lifecycle_faults -- --test-threads=1

# Gate A/D: first reverify the immutable image-built candidate used by the
# cross-UID probe. Build a fresh reproduction in a distinct validation target,
# require exact mode/size/hash/bytes equivalence across the two planes, then
# compile test harnesses elsewhere while executing only the frozen candidate.
run_gate release_candidate_binding \
  "$evidence" binary-verify "$initial_binaries" --output "$candidate_before"
release_candidate_binding_status=$LAST_GATE_STATUS
if [[ $release_candidate_binding_status -eq 0 ]]; then
  run_gate release_build \
    env CARGO_TARGET_DIR="$repro_release_target" \
    cargo +1.88 build --locked --workspace --release --bins
  release_build_status=$LAST_GATE_STATUS
else
  release_build_status=1
  skip_gate release_build
fi
if [[ $release_build_status -eq 0 ]]; then
  run_gate release_repro_stage \
    "$evidence" binary-repro-stage \
    "$repro_release_target/release" "$repro_bin" "$repro_stage_manifest"
  release_repro_stage_status=$LAST_GATE_STATUS
else
  release_repro_stage_status=1
  skip_gate release_repro_stage
fi
if [[ $release_repro_stage_status -eq 0 ]]; then
  run_gate release_repro_binary_equivalence \
    "$evidence" binary-repro-compare \
    "$candidate_before" "$repro_bin" "$repro_comparison"
  release_candidate_status=$LAST_GATE_STATUS
else
  release_candidate_status=1
  skip_gate release_repro_binary_equivalence
fi

if [[ $release_candidate_status -eq 0 ]]; then
  run_gate typescript_stage_preconsume_unchanged \
    bash "$suite_script" --verify-typescript-stage-identity \
    "$typescript_stage_digest" \
    node \
    "$workspace/clients/typescript/tests/dist-stage.mjs" \
    "$typescript_dist" \
    "$workspace"
  typescript_stage_preconsume_status=$LAST_GATE_STATUS
else
  typescript_stage_preconsume_status=1
  skip_gate typescript_stage_preconsume_unchanged
fi

if [[ $release_candidate_status -eq 0 && $typescript_stage_preconsume_status -eq 0 ]]; then
  run_gate release_full_stack_e2e \
    env \
    PMUX_E2E_BIN_DIR="$release_dir" \
    PMUX_E2E_TYPESCRIPT_DIST_DIR="$typescript_dist" \
    cargo +1.88 test --locked -p pseudomux-e2e --all-targets \
    -- --ignored --test-threads=1
  run_gate cli_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmux --all-targets -- --test-threads=1
  run_gate mcp_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmux-mcp --test stdio_blackbox
  run_gate facade_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p claude-p --test facade_blackbox
  run_gate launcher_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmux-launcher --test process_blackbox
  run_gate hook_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmux-hook --test process_blackbox
  run_gate rmuxd_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmux-rmuxd --test process_blackbox
  run_gate pmuxd_process \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pmuxd --test process_blackbox

  # Repeat process/resource gates whose test support explicitly accepts the
  # frozen candidate directory, rather than silently exercising debug helpers.
  run_gate release_native_service \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test native_service -- --ignored --test-threads=1
  run_gate release_private_runtime \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test private_runtime -- --ignored --test-threads=1
  run_gate release_lifecycle_faults \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test lifecycle_faults -- --test-threads=1
else
  skip_gate release_full_stack_e2e
  skip_gate cli_process
  skip_gate mcp_process
  skip_gate facade_process
  skip_gate launcher_process
  skip_gate hook_process
  skip_gate rmuxd_process
  skip_gate pmuxd_process
  skip_gate release_native_service
  skip_gate release_private_runtime
  skip_gate release_lifecycle_faults
fi

# Gate A/E: deterministic concurrency, capacity, resource, soak, and scaling.
run_gate concurrency_backpressure \
  cargo +1.88 test --locked -p pseudomux-service --test concurrency_backpressure
run_gate resource_bounds \
  cargo +1.88 test --locked -p pseudomux-service \
  --test resource_bounds -- --test-threads=1
run_gate bounded_soak \
  cargo +1.88 test --locked -p pseudomux-service \
  --test bounded_soak -- --test-threads=1
run_gate replay_scaling \
  cargo +1.88 test --locked -p pseudomux-service \
  --lib replay_scaling_tests -- --test-threads=1
run_gate protocol_framing_scaling \
  cargo +1.88 test --locked -p pmuxd --bin pmuxd \
  native_framing_and_successful_decode_have_deterministic_linear_work \
  -- --test-threads=1
run_gate transcript_size_scaling \
  cargo +1.88 test --locked -p pseudomux-claude --test size_scaling --release \
  -- --nocapture --test-threads=1
if [[ $release_candidate_status -eq 0 ]]; then
  run_gate release_concurrency_backpressure \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test concurrency_backpressure -- --include-ignored --test-threads=1
  run_gate release_resource_bounds \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test resource_bounds -- --include-ignored --test-threads=1
  run_gate release_bounded_soak \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked -p pseudomux-service \
    --test bounded_soak -- --test-threads=1
  run_gate release_performance_diagnostics \
    env PMUX_TEST_BIN_DIR="$release_dir" \
    cargo +1.88 test --locked --release -p pseudomux-service \
    --test performance_diagnostics -- --nocapture --test-threads=1
  run_gate release_binary_unchanged \
    "$evidence" binary-verify "$candidate_before" --output "$candidate_after"
else
  skip_gate release_concurrency_backpressure
  skip_gate release_resource_bounds
  skip_gate release_bounded_soak
  skip_gate release_performance_diagnostics
  skip_gate release_binary_unchanged
fi

# Gate A/F: tooling and evidence-envelope self-tests run after every product
# layer, matching the tracked command manifest exactly.
run_gate evidence_common_tests \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/evidence_common/tests -v
run_gate package_smoke_self_tests \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/package-smoke/tests -v
run_gate phase0_evidence_tests \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/phase0/tests -v
run_gate candidate_envelope_tests \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/gate-a-candidate/tests -v
run_gate shell_syntax \
  bash -n \
  scripts/pmuxd-run.sh \
  scripts/gate-a-fuzz.sh \
  scripts/gate-a-residue.sh \
  tools/linux-docker/run.sh \
  tools/linux-docker/inside.sh \
  tools/linux-docker/suite.sh
run_gate shellcheck \
  shellcheck \
  scripts/pmuxd-run.sh \
  scripts/gate-a-fuzz.sh \
  scripts/gate-a-residue.sh \
  tools/linux-docker/run.sh \
  tools/linux-docker/inside.sh \
  tools/linux-docker/suite.sh
run_gate linux_runner_tests \
  env PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/linux-docker/tests -v
run_gate typescript_stage_postconsume_unchanged \
  bash "$suite_script" --verify-typescript-stage-identity \
  "$typescript_stage_digest" \
  node \
  "$workspace/clients/typescript/tests/dist-stage.mjs" \
  "$typescript_dist" \
  "$workspace"
run_gate validation_output_cleanup \
  bash "$suite_script" --cleanup-validation-outputs "$validation_root"
run_gate residue_script_self_test \
  bash scripts/gate-a-residue.sh --self-test-disappearing-temp-root
if [[ $release_candidate_status -eq 0 ]]; then
  run_gate gate_a_residue \
    env PMUX_E2E_BIN_DIR="$release_dir" \
    bash scripts/gate-a-residue.sh
else
  skip_gate gate_a_residue
fi

# Recompute the canonical source after every gate and prove exact equality to
# both the image/host freeze and the pre-gate container manifest.
run_gate container_source_after \
  "$workspace/tools/linux-docker/source_digest.py" "$workspace" \
  --expected "$frozen_source" \
  --json \
  --output "$artifacts/container-source-after.json"
run_gate container_source_stability \
  "$evidence" source-stability \
  "$artifacts/container-source-before.json" \
  "$artifacts/container-source-after.json" \
  "$frozen_source" \
  "$artifacts/container-source-stability.json"
run_gate artifact_privacy "$evidence" secure-tree "$artifacts"

"$evidence" suite-result \
  "$summary" "$failures" "$platform_gate_manifest" \
  "$gate_evidence_ledger" "$gate_evidence_ordinal" "$gate_evidence_anchor" \
  "$artifacts/result.json"
"$evidence" secure-tree "$artifacts"
"$evidence" tree-manifest \
  "$artifacts" "$artifacts/container-artifact-tree-final.json"
"$evidence" secure-tree "$artifacts"

if [[ $failures -ne 0 ]]; then
  echo "linux-docker: $failures gate(s) failed" >&2
  exit 1
fi
echo "linux-docker: all platform-applicable deterministic gates passed"
