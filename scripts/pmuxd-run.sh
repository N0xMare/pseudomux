#!/usr/bin/env bash
set -euo pipefail

# Development supervisor. It builds every runtime companion into the same
# directory before starting pmuxd. For a release installation, keep pmuxd,
# pmux-rmuxd, pmux-launcher, and pmux-hook adjacent:
#   cargo build --workspace --release
#   ./target/release/pmuxd serve --socket /absolute/path/pmux.sock [args]

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ -n "${PSEUDOMUX_STATE_DIR:-}" ]]; then
  STATE_DIR="$PSEUDOMUX_STATE_DIR"
elif [[ "${XDG_STATE_HOME:-}" ]]; then
  STATE_DIR="$XDG_STATE_HOME/pseudomux"
else
  STATE_DIR="$HOME/.local/state/pseudomux"
fi
LOG_DIR="${PSEUDOMUX_LOG_DIR:-$STATE_DIR/logs}"
LOG_FILE="${PSEUDOMUX_SUPERVISOR_LOG:-$LOG_DIR/pmuxd-supervisor.log}"
LOCK_DIR_ROOT="${PSEUDOMUX_LOCK_DIR:-$STATE_DIR/locks}"
RESTART_DELAY="${PSEUDOMUX_RESTART_DELAY_SEC:-2}"
LOCK_WAIT_SEC="${PSEUDOMUX_LOCK_WAIT_SEC:-20}"
SOCKET_DEFAULT="${PMUX_SOCKET:-${PSEUDOMUX_SOCKET:-$STATE_DIR/pmux.sock}}"

mkdir -p "$LOG_DIR" "$LOCK_DIR_ROOT"
chmod 700 "$STATE_DIR" "$LOG_DIR" "$LOCK_DIR_ROOT" 2>/dev/null || true

resolve_socket() {
  local socket="$SOCKET_DEFAULT"
  local args=("$@")
  local i=0
  while [[ $i -lt ${#args[@]} ]]; do
    if [[ "${args[$i]}" == "--socket" ]] && [[ $((i + 1)) -lt ${#args[@]} ]]; then
      socket="${args[$((i + 1))]}"
      break
    fi
    if [[ "${args[$i]}" == --socket=* ]]; then
      socket="${args[$i]#--socket=}"
      break
    fi
    i=$((i + 1))
  done
  printf '%s\n' "$socket"
}

SOCKET_PATH="$(resolve_socket "$@")"
if command -v md5sum >/dev/null 2>&1; then
  LOCK_KEY="$(printf '%s' "$SOCKET_PATH" | md5sum | awk '{print $1}')"
elif command -v md5 >/dev/null 2>&1; then
  LOCK_KEY="$(printf '%s' "$SOCKET_PATH" | md5 -q)"
else
  LOCK_KEY="$(printf '%s' "$SOCKET_PATH" | shasum -a 256 | awk '{print $1}')"
fi
LOCK_DIR="$LOCK_DIR_ROOT/pmuxd-$LOCK_KEY.lock"
LOCK_PID_FILE="$LOCK_DIR/pid"

DAEMON_ARGS=("$@")
if ! printf '%s\n' "$@" | grep -Eq '^--socket($|=)'; then
  DAEMON_ARGS=(--socket "$SOCKET_PATH" "${DAEMON_ARGS[@]}")
fi

log_event() {
  local event="$1"
  local extra="${2:-}"
  printf '{"event":"%s","socket":"%s","pid":%s%s,"ts":"%s"}\n' \
    "$event" "$SOCKET_PATH" "$$" "$extra" "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" >>"$LOG_FILE"
}

socket_is_live() {
  local path="$1"
  # Check the socket file exists first
  [[ -S "$path" ]] || return 1
  # Attempt a real connection to confirm pmuxd is listening.
  # Prefer socat (reliable Unix socket connect); fall back to file-existence only.
  if command -v socat >/dev/null 2>&1; then
    socat -T 1 /dev/null UNIX-CONNECT:"$path" 2>/dev/null
  else
    # socat not available — file existence is our best check.
    # pmuxd removes the socket on clean shutdown, so stale sockets are rare.
    return 0
  fi
}

cleanup_stale_socket() {
  local path="$1"
  if [[ -S "$path" ]] && [[ -O "$path" ]] && ! socket_is_live "$path"; then
    rm -f "$path"
    log_event "stale_socket_removed"
  elif [[ -S "$path" ]] && [[ ! -O "$path" ]] && ! socket_is_live "$path"; then
    log_event "stale_socket_foreign_owner"
  fi
}

release_lock() {
  if [[ -d "$LOCK_DIR" ]] && [[ -f "$LOCK_PID_FILE" ]] && [[ "$(cat "$LOCK_PID_FILE" 2>/dev/null || true)" == "$$" ]]; then
    rm -rf "$LOCK_DIR"
  fi
}

acquire_lock() {
  local started_at
  started_at="$(date +%s)"
  while true; do
    if mkdir "$LOCK_DIR" 2>/dev/null; then
      printf '%s\n' "$$" >"$LOCK_PID_FILE"
      log_event "lock_acquired"
      return 0
    fi

    local owner_pid=""
    owner_pid="$(cat "$LOCK_PID_FILE" 2>/dev/null || true)"
    if [[ -n "$owner_pid" ]] && kill -0 "$owner_pid" 2>/dev/null; then
      if socket_is_live "$SOCKET_PATH"; then
        log_event "already_running"
        return 1
      fi
      local now
      now="$(date +%s)"
      if (( now - started_at >= LOCK_WAIT_SEC )); then
        log_event "lock_wait_timeout" ",\"owner_pid\":$owner_pid,\"wait_sec\":$LOCK_WAIT_SEC"
        return 1
      fi
      sleep 0.2
      continue
    fi

    rm -rf "$LOCK_DIR" || true
    sleep 0.1
  done
}

if ! acquire_lock; then
  exit 0
fi
trap release_lock EXIT

if socket_is_live "$SOCKET_PATH"; then
  log_event "already_running"
  exit 0
fi
cleanup_stale_socket "$SOCKET_PATH"

log_event "build_companions"
cargo build -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook >>"$LOG_FILE" 2>&1
PMUXD_BIN="$ROOT_DIR/target/debug/pmuxd"

while true; do
  log_event "start"
  if "$PMUXD_BIN" serve "${DAEMON_ARGS[@]}" >>"$LOG_FILE" 2>&1; then
    code=0
  else
    code=$?
  fi
  if [[ $code -eq 0 ]]; then
    log_event "exit" ",\"code\":0"
    exit 0
  fi
  if socket_is_live "$SOCKET_PATH"; then
    log_event "listener_detected_after_failure" ",\"code\":$code"
    exit 0
  fi
  cleanup_stale_socket "$SOCKET_PATH"
  log_event "restart" ",\"code\":$code,\"delay_sec\":$RESTART_DELAY"
  sleep "$RESTART_DELAY"
done
