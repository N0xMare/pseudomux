#!/usr/bin/env bash
# Run one long gate against a PINNED COMMIT in its own worktree, so the tree you
# are editing stops being the tree the gate is grading.
#
# WHY
#
# The mutation gate is ~3 hours and Gate A is ~35 minutes, and both require a
# frozen tree: `run_gate.py` hashes the source before and after and reports
# `source_unchanged`, and `cargo-mutants` copies the tree it finds. So today the
# two long gates serialize against ALL editing, which is the single largest
# wall-clock cost in this project. This script puts the gate somewhere else:
# `git worktree add --detach` at an explicit commit, run the gate there, write a
# receipt, remove the worktree.
#
# WHAT THE RECEIPT IS FOR, AND THE DEFECT IT EXISTS AGAINST
#
# A gate receipt written by `tools/gate-a/run_gate.py` names no commit. Read
# beside a repository, it is silently taken to describe HEAD -- and this
# repository keeps finding exactly that defect: a 61/62 receipt at one commit
# quoted as current seven commits later, a mutation score from thirty-six
# commits back briefed as this tree's. A receipt for an ancestor is worth having
# and is not worth guessing about, so this one says, in three fields and in its
# last printed line, WHICH COMMIT IT DESCRIBES and whether that commit was HEAD
# when it was written. Every artefact the run produced is hashed into it, so the
# gate receipt inside it is identified by content and not by a path someone can
# overwrite.
#
# WHAT IT DOES NOT CHANGE
#
# Nothing about what any gate measures. The gate command is given on the command
# line and run verbatim, with `{worktree}`, `{artefacts}`, `{validation}` and
# `{commit}` substituted. The worktree is a full checkout of the commit and the
# gate writes into its own `target/` exactly as it does in the main tree, so
# `target/` sharing is not merely uncorrupted -- there is none: two cargo
# processes in two target directories do not queue behind one lock, which is the
# blocking this exists to remove.
#
# USAGE
#
#   bash scripts/gate-in-worktree.sh --commit HEAD --label gate-a -- \
#     python3 {worktree}/tools/gate-a/run_gate.py \
#       --manifest {worktree}/tools/gate-a-candidate/phase-manifest.json \
#       --workspace {worktree} --release-dir {worktree}/target/release \
#       --validation-root {validation} \
#       --receipt {artefacts}/gate-a-receipt.json --phase gate_a
#
# `--release-build` runs `cargo build --locked --release --workspace` in the
# worktree first, and `--prepare '<shell command>'` (repeatable) runs anything
# else the manifest declares as a precondition and no cell performs. Gate A has
# exactly two: that release build, and `clients/typescript/node_modules` from
# the locked `npm ci`, which `docs/testing.md` requires to exist already. A
# fresh checkout has neither -- MEASURED, by a first run of this script that
# reddened four `gate_a` typescript cells for a reason that was about the
# checkout and not about the commit. `--keep` leaves the worktree for a
# post-mortem.
#
# WHERE THE RECEIPT GOES, AND WHY IT IS NOT A FREE CHOICE
#
# MEASURED, and the whole reason this section exists: a certification ran both
# gates in pinned worktrees and passed `--receipt` an ephemeral path. The runs
# were fine. The receipt died with the work root, only the raw gate receipts
# were copied out by hand, and `scripts/path-b-done.sh` then reported criterion
# 4 NOT MET with `cells_executed=0` over 62 cells that had really run -- because
# a bare `run_gate.py` receipt names no commit and is refused, by design. No
# pinned receipt for that commit existed anywhere on disk.
#
# So `--receipt` DEFAULTS to `.context/gate-a/pinned-receipt-<label>-<commit>.json`
# under the repository, and a path that cannot outlive the run it describes is
# REFUSED before the checkout: anything under the directory
# `tempfile.gettempdir()` names -- computed twice, with and without this
# environment's `TMPDIR`, `TMP` and `TEMP`, so the platform default is refused
# even when the shell overrides it -- and anything under `--work-root`, whose
# checkout this script removes and whose work directory sits under a reaped root
# by default. A receipt inside the repository must additionally be at a path
# `git check-ignore` accepts: one that dirties the tree makes the done-gate
# refuse to decide at all.
#
# `--print-receipt-path` prints the path a run with these arguments would write,
# and exits without creating anything. It applies every rule above first, so it
# refuses rather than printing a path the run beside it would reject. That is
# how `scripts/path_b_done.py` names the file it wants without keeping a second
# copy of this convention.
#
# THE EVIDENCE MOVES WITH THE RECEIPT
#
# A receipt hashes files, so a receipt whose files are reaped names evidence
# nobody can re-check. The two logs and everything under `artefacts/` are
# therefore COPIED beside the receipt into `<receipt>.evidence/`, every copy
# compared to its original by digest, and the copies are what the receipt
# hashes. If any of that fails the receipt still lands -- with
# `evidence_durable` false, `evidence_fault` saying what happened, and the
# work directory's paths hashed instead. A receipt that states its evidence is
# perishable is worth having; one that pretends otherwise is not.
#
# The CHECKOUT is removed when the run ends. The work directory beside it keeps
# the originals for a post-mortem and can be deleted once the receipt is
# written, which is the difference this section buys.
#
#   exit 0  the gate command exited 0, or --print-receipt-path printed a path
#   exit 1  the gate command exited non-zero (its status is in the receipt)
#   exit 2  the runner could not start, could not write the receipt, or refused
#           the receipt path it was given

set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_DIR
readonly RECEIPT_SCHEMA="pmux.pinned-worktree-run.v1"
# Why this receipt spells absolute paths where `evidence/`'s receipts spell
# `<REPO>`, `<HOME>` and `<TMPDIR>` (`tools/evidence_common/portable_paths.py`),
# stated IN the artefact because a reader who meets one of each is owed the
# difference. Here a path is a HANDLE and not a description:
# `scripts/path_b_done.py` opens every `artefacts[].path`, re-hashes it against
# the digest recorded beside it, and then compares the gate receipt's own
# `workspace` with the `worktree` below. Those two are written by two processes
# in two different checkouts -- the driver runs inside the pinned worktree, this
# runner beside it -- so each would render its own `<REPO>` and two spellings of
# one directory would stop comparing equal.
readonly ABSOLUTE_PATHS_ARE_THE_CONTRACT="absolute on purpose: scripts/path_b_done.py re-opens the artefacts named below and compares the gate receipt's workspace with this worktree, which is written by a different process in a different checkout"
# The one place this convention is written down. `scripts/path_b_done.py` asks
# for it with `--print-receipt-path` rather than spelling it a second time,
# because a naming rule with two authors is a rule with two answers.
readonly RECEIPT_DIR=".context/gate-a"

COMMIT="HEAD"
RECEIPT=""
WORK_ROOT="${PMUX_WORKTREE_ROOT:-${TMPDIR:-/tmp}/gate-worktrees}"
LABEL="gate"
RELEASE_BUILD=0
KEEP=0
PRINT_RECEIPT_PATH=0
PREPARE=()
GATE_PID=""
EVIDENCE_DIR=""
EVIDENCE_DURABLE=false
EVIDENCE_FAULT=""

usage() {
  # The header itself, derived: every leading comment line below the shebang,
  # ending where the comments do. A line range written here would name the
  # length this file had on the day it was written.
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' \
    "${BASH_SOURCE[0]}" >&2
}

while (($#)); do
  case "$1" in
    --commit) COMMIT="${2:?--commit needs a revision}"; shift 2 ;;
    --receipt) RECEIPT="${2:?--receipt needs a path}"; shift 2 ;;
    --work-root) WORK_ROOT="${2:?--work-root needs a path}"; shift 2 ;;
    --label) LABEL="${2:?--label needs a name}"; shift 2 ;;
    --release-build) RELEASE_BUILD=1; shift ;;
    --prepare) PREPARE+=("${2:?--prepare needs a command}"); shift 2 ;;
    --keep) KEEP=1; shift ;;
    --print-receipt-path) PRINT_RECEIPT_PATH=1; shift ;;
    --) shift; break ;;
    -h|--help) usage; exit 2 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

canonical_path() {
  PMUX_CANONICAL_INPUT="$1" python3 -c '
import os
import pathlib
print(pathlib.Path(os.environ["PMUX_CANONICAL_INPUT"]).resolve())
'
}

if ! COMMIT_SHA="$(git -C "$REPO_DIR" rev-parse --verify "${COMMIT}^{commit}" 2>/dev/null)"; then
  echo "not a commit in this repository: $COMMIT" >&2
  exit 2
fi
readonly COMMIT_SHA

# The durable default, so that a caller who says nothing gets evidence rather
# than oblivion. It is inside the tree only because `.context/` is ignored, and
# that is checked below rather than assumed.
if [[ -z "$RECEIPT" ]]; then
  RECEIPT="$REPO_DIR/$RECEIPT_DIR/pinned-receipt-$LABEL-${COMMIT_SHA:0:7}.json"
fi
if [[ "$RECEIPT" != /* ]]; then
  echo "--receipt must be absolute: $RECEIPT" >&2
  exit 2
fi
# Canonical for the reason `--work-root` is: `/tmp` and `/var` are symlinks on
# macOS, so a durability rule that compared the string a caller typed would be
# answering about a spelling rather than about a directory.
RECEIPT="$(canonical_path "$RECEIPT")"

if [[ "$WORK_ROOT" != /* ]]; then
  echo "--work-root must be absolute: $WORK_ROOT" >&2
  exit 2
fi
# Canonical before anything is decided about it, and resolved rather than
# entered, because `--print-receipt-path` answers below without creating a
# thing. `/tmp` and `/var` are both symlinks on macOS, so the spelling a caller
# passes and the path a gate reads back out of a receipt are routinely two
# different strings for one directory.
WORK_ROOT="$(canonical_path "$WORK_ROOT")"
case "$WORK_ROOT/" in
  "$REPO_DIR"/*)
    echo "--work-root must not be inside the repository ($REPO_DIR): a second checkout \
under the tree you are editing is inside the source digest, the residue audit and every \
glob the gates use" >&2
    exit 2
    ;;
esac

# WHERE the worktree sits changes what two gate cells measure, which is the one
# thing this script may not do. `tools/linux-docker/evidence.py` opens EVERY
# absolute path component of the file it is about to read, with `O_NOFOLLOW`,
# and refuses any component carrying setuid, setgid or the sticky bit. `/tmp` is
# mode 1777. MEASURED: a run rooted there came back 59/62 with
# `gate_f/candidate_envelope_self_tests` at `FAILED (failures=14, errors=9)` and
# `gate_f/linux_docker_self_tests` red beside it, every message reading *"JSON
# evidence parent has unsupported special mode bits"* -- 84 s and 145 s of
# failure, 50 minutes into a 39-minute run, about the directory the worktree was
# put in and not about the commit. So the chain is checked before the checkout,
# and a symlinked component is refused for the same reason: `O_NOFOLLOW` will
# not traverse one.
#
# This also settles the residue question by subtraction, which is why no rule
# here names a directory: `scripts/gate-a-residue.sh` scans one level under
# `/tmp` for a leaked test root, and every path under `/tmp` crosses `/private/tmp`
# at mode 1777, so no worktree can be there to be mistaken for one.
special_mode_bits_in_chain() {
  PMUX_GATE_WORKTREE_ROOT="$1" python3 - <<'PY'
import os
import pathlib
import stat

root = pathlib.Path(os.environ["PMUX_GATE_WORKTREE_ROOT"])
walked = pathlib.Path(root.root)
for component in root.parts[1:]:
    walked = walked / component
    try:
        mode = walked.lstat().st_mode
    except OSError:
        break  # not created yet; every component below is this script's own
    if stat.S_ISLNK(mode):
        print(f"{walked} is a symlink, which O_NOFOLLOW will not traverse")
    elif stat.S_IMODE(mode) & 0o7000:
        print(f"{walked} is mode {stat.S_IMODE(mode):04o}")
PY
}

# A receipt is evidence only if it outlives the run it describes, and a caller
# who names a doomed path finds out ~40 minutes later that the measurement is
# gone. So the roots that do not outlive a run are refused HERE, before the
# checkout -- and they are asked for rather than listed: `tempfile.gettempdir()`
# answers what "temporary" means on this host, and answering it a second time
# with `TMPDIR`, `TMP` and `TEMP` removed from the environment yields the
# platform default underneath whatever this shell set. The work root is the
# third, and it is this run's own: `finalize` removes the checkout under it, and
# by default it sits under the first two anyway.
ephemeral_root_of() {
  PMUX_RECEIPT="$1" PMUX_WORK_ROOT="$2" python3 - <<'PY'
import os
import pathlib
import tempfile

receipt = pathlib.Path(os.environ["PMUX_RECEIPT"])
roots: list[tuple[pathlib.Path, str]] = []


def consider(where: str, why: str) -> None:
    if where:
        roots.append((pathlib.Path(where).resolve(), why))


consider(tempfile.gettempdir(), "the temporary directory this environment names")
for variable in ("TMPDIR", "TMP", "TEMP"):
    consider(os.environ.pop(variable, ""), f"${variable}")
# Recomputed with those removed: `gettempdir` caches its first answer, so the
# reset is what makes the second call report the platform default rather than
# repeat the first.
tempfile.tempdir = None
consider(tempfile.gettempdir(), "the platform default temporary directory")
consider(
    os.environ["PMUX_WORK_ROOT"],
    "--work-root, whose checkout this run removes when it ends",
)

for root, why in roots:
    if receipt == root or root in receipt.parents:
        print(f"{root}\t{why}")
        break
PY
}
if ! receipt_root="$(ephemeral_root_of "$RECEIPT" "$WORK_ROOT")"; then
  echo "could not decide whether $RECEIPT outlives this run" >&2
  exit 2
fi
if [[ -n "$receipt_root" ]]; then
  echo "--receipt is under ${receipt_root%%$'\t'*} (${receipt_root#*$'\t'}), so it does \
not outlive the run it describes and is not evidence: $RECEIPT" >&2
  echo "pass no --receipt at all and this run writes \
$REPO_DIR/$RECEIPT_DIR/pinned-receipt-$LABEL-${COMMIT_SHA:0:7}.json" >&2
  exit 2
fi
# A receipt the repository tracks dirties the tree, and a dirty tree is what
# `scripts/path-b-done.sh` refuses to give any verdict from -- so a receipt
# written there costs the whole gate, not just itself. Asked of git rather than
# compared against a directory name, because the ignore rules are git's.
case "$RECEIPT" in
  "$REPO_DIR"/*)
    if ! git -C "$REPO_DIR" check-ignore -q "$RECEIPT"; then
      echo "--receipt is inside the repository at a path git does not ignore, so writing \
it would dirty the tree the next gate grades: $RECEIPT" >&2
      exit 2
    fi
    ;;
esac

# Answered only once the path has passed every rule a run applies to it. A
# query that printed a path the run beside it would refuse would be a worse
# answer than no answer: `scripts/path_b_done.py` prints this as the file it
# wants, and it has to be a file somebody can actually get.
if ((PRINT_RECEIPT_PATH)); then
  printf '%s\n' "$RECEIPT"
  exit 0
fi

if (($# == 0)); then
  echo "no gate command was given after --" >&2
  exit 2
fi
if ! mkdir -p "$(dirname "$RECEIPT")"; then
  echo "could not create the directory for --receipt: $(dirname "$RECEIPT")" >&2
  exit 2
fi
mkdir -p "$WORK_ROOT"
chmod 700 "$WORK_ROOT"
if ! chain_faults="$(special_mode_bits_in_chain "$WORK_ROOT")"; then
  echo "could not read the mode bits of $WORK_ROOT" >&2
  exit 2
fi
if [[ -n "$chain_faults" ]]; then
  echo "--work-root cannot be reached without crossing a directory the candidate \
envelope refuses, so two gate_f cells would go red about the checkout rather than about \
the commit:" >&2
  echo "$chain_faults" >&2
  exit 2
fi

TREE_SHA="$(git -C "$REPO_DIR" rev-parse "${COMMIT_SHA}^{tree}")"
readonly TREE_SHA
COMMIT_SUBJECT="$(git -C "$REPO_DIR" log -1 --format=%s "$COMMIT_SHA")"
readonly COMMIT_SUBJECT
HEAD_AT_START="$(git -C "$REPO_DIR" rev-parse HEAD)"
readonly HEAD_AT_START
DIRTY_AT_START=false
if [[ -n "$(git -C "$REPO_DIR" status --porcelain)" ]]; then
  DIRTY_AT_START=true
fi
readonly DIRTY_AT_START

WORK_DIR="$(mktemp -d "$WORK_ROOT/${LABEL}.${COMMIT_SHA:0:7}.XXXXXX")"
readonly WORK_DIR
readonly WORKTREE="$WORK_DIR/tree"
readonly ARTEFACTS="$WORK_DIR/artefacts"
readonly VALIDATION="$WORK_DIR/validation"
mkdir -p "$ARTEFACTS" "$VALIDATION"
chmod 700 "$WORK_DIR" "$ARTEFACTS" "$VALIDATION"

COMMAND=()
for argument in "$@"; do
  argument="${argument//\{worktree\}/$WORKTREE}"
  argument="${argument//\{artefacts\}/$ARTEFACTS}"
  argument="${argument//\{validation\}/$VALIDATION}"
  argument="${argument//\{commit\}/$COMMIT_SHA}"
  # Fail closed on a placeholder nobody expanded, exactly as the gate driver
  # does: an unexpanded `{name}` is a path that does not exist, and a gate that
  # runs against one measures nothing and says nothing.
  if [[ "$argument" =~ \{[a-z_]+\} ]]; then
    echo "unresolved placeholder in the gate command: $argument" >&2
    rm -rf "$WORK_DIR"
    exit 2
  fi
  COMMAND+=("$argument")
done

STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
START_EPOCH="$(date -u +%s)"
readonly STARTED_AT START_EPOCH
BUILD_STATUS="null"
BUILD_SECONDS=0
GATE_STATUS=2
WORKTREE_REMOVED=false

hash_file() {
  shasum -a 256 "$1" 2>/dev/null | awk '{print $1}'
}

# `null`, never an empty field. MEASURED, by leaving one artefact at mode 000:
# `shasum` and `wc` both refused it, the receipt went out carrying
# `"sha256": "", "bytes": ` and stopped being JSON any reader could parse -- so
# the second failure hid the first, which was the one worth reading. A receipt
# that says it could not read a file is evidence; a receipt that will not parse
# is not.
json_digest() {
  local digest
  digest="$(hash_file "$1")"
  if [[ -n "$digest" ]]; then printf '"%s"' "$digest"; else printf 'null'; fi
}

json_bytes() {
  local bytes
  # Braced: a redirection that cannot open its file is reported by the SHELL,
  # not by `wc`, so `wc ... 2>/dev/null` alone still prints a line naming this
  # script and a line number at the one moment the receipt is being written.
  bytes="$({ wc -c <"$1" | tr -d ' '; } 2>/dev/null)"
  if [[ -n "$bytes" ]]; then printf '%s' "$bytes"; else printf 'null'; fi
}

# The receipt hashes files. Left in the work directory those files sit under a
# reaped root, so a receipt that survives ends up naming evidence that did not
# -- which `scripts/path_b_done.py` reports as "names an artefact that is gone",
# correctly and uselessly. Copying them beside the receipt makes the two halves
# share a fate. Every copy is compared to its original by digest, because a
# short write that nobody checked would put a digest of the wrong bytes into a
# document whose only job is to be re-checkable.
publish_evidence() {
  local origin relative
  EVIDENCE_DIR="${RECEIPT%.json}.evidence"
  if ! rm -rf "$EVIDENCE_DIR" || ! mkdir -p "$EVIDENCE_DIR/artefacts"; then
    EVIDENCE_FAULT="could not create $EVIDENCE_DIR"
    return 0
  fi
  chmod 700 "$EVIDENCE_DIR" "$EVIDENCE_DIR/artefacts"
  for relative in stdout.log stderr.log; do
    if [[ -f "$WORK_DIR/$relative" ]] &&
      ! cp "$WORK_DIR/$relative" "$EVIDENCE_DIR/$relative"; then
      EVIDENCE_FAULT="could not copy $relative into $EVIDENCE_DIR"
      return 0
    fi
  done
  if [[ -d "$ARTEFACTS" ]] && ! cp -R "$ARTEFACTS/." "$EVIDENCE_DIR/artefacts/"; then
    EVIDENCE_FAULT="could not copy the artefacts into $EVIDENCE_DIR"
    return 0
  fi
  local origin_digest
  while IFS= read -r origin; do
    [[ -z "$origin" ]] && continue
    relative="${origin#"$ARTEFACTS"/}"
    # An unreadable original is a fault in its own right and not merely an
    # unequal comparison: `hash_file` answers the empty string for both sides
    # when neither can be read, and two failures that agree would otherwise
    # pass for a copy that matched.
    origin_digest="$(hash_file "$origin")"
    if [[ -z "$origin_digest" ]] ||
      [[ "$origin_digest" != "$(hash_file "$EVIDENCE_DIR/artefacts/$relative")" ]]; then
      EVIDENCE_FAULT="$relative did not copy intact into $EVIDENCE_DIR"
      return 0
    fi
  done < <(find "$ARTEFACTS" -type f 2>/dev/null | LC_ALL=C sort)
  EVIDENCE_DURABLE=true
}

# The file the receipt should hash: the durable copy when there is one, and the
# work directory's original when there is not. One function so that no field of
# the receipt can disagree with `evidence_durable` about which tree it read.
durable_copy_of() {
  local original=$1 relative
  if ! $EVIDENCE_DURABLE; then
    printf '%s' "$original"
    return 0
  fi
  case "$original" in
    "$ARTEFACTS"/*)
      relative="${original#"$ARTEFACTS"/}"
      printf '%s' "$EVIDENCE_DIR/artefacts/$relative"
      ;;
    *) printf '%s' "$EVIDENCE_DIR/$(basename "$original")" ;;
  esac
}

write_receipt() {
  local finished_at elapsed head_now describes_head warning
  finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  elapsed=$(($(date -u +%s) - START_EPOCH))
  head_now="$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
  if [[ "$head_now" == "$COMMIT_SHA" ]]; then
    describes_head=true
    warning="this receipt describes $COMMIT_SHA, which was HEAD when it was written"
  else
    describes_head=false
    warning="THIS RECEIPT DESCRIBES $COMMIT_SHA AND NOT HEAD ($head_now). Nothing in it \
is a statement about any other commit."
  fi
  if ! $EVIDENCE_DURABLE; then
    warning="$warning THE FILES THIS RECEIPT HASHES ARE IN THE WORK DIRECTORY AND WILL \
NOT OUTLIVE IT ($EVIDENCE_FAULT); copy them somewhere durable or re-run."
  fi
  mkdir -p "$(dirname "$RECEIPT")"
  {
    printf '{\n'
    printf '  "schema": "%s",\n' "$RECEIPT_SCHEMA"
    printf '  "describes_commit": "%s",\n' "$COMMIT_SHA"
    printf '  "describes_commit_subject": %s,\n' "$(json_string "$COMMIT_SUBJECT")"
    printf '  "tree_sha": "%s",\n' "$TREE_SHA"
    printf '  "describes_head": %s,\n' "$describes_head"
    printf '  "reader_warning": %s,\n' "$(json_string "$warning")"
    printf '  "head_at_start": "%s",\n' "$HEAD_AT_START"
    printf '  "head_at_finish": "%s",\n' "$head_now"
    printf '  "main_tree_dirty_at_start": %s,\n' "$DIRTY_AT_START"
    printf '  "label": %s,\n' "$(json_string "$LABEL")"
    printf '  "paths_are_absolute_because": %s,\n' \
      "$(json_string "$ABSOLUTE_PATHS_ARE_THE_CONTRACT")"
    printf '  "repository": %s,\n' "$(json_string "$REPO_DIR")"
    printf '  "worktree": %s,\n' "$(json_string "$WORKTREE")"
    printf '  "worktree_removed": %s,\n' "$WORKTREE_REMOVED"
    printf '  "artefacts_dir": %s,\n' "$(json_string "$ARTEFACTS")"
    printf '  "evidence_dir": %s,\n' \
      "$($EVIDENCE_DURABLE && json_string "$EVIDENCE_DIR" || printf 'null')"
    printf '  "evidence_durable": %s,\n' "$EVIDENCE_DURABLE"
    printf '  "evidence_fault": %s,\n' \
      "$([[ -n "$EVIDENCE_FAULT" ]] && json_string "$EVIDENCE_FAULT" || printf 'null')"
    printf '  "validation_root": %s,\n' "$(json_string "$VALIDATION")"
    printf '  "release_build": %s,\n' "$BUILD_STATUS"
    printf '  "release_build_seconds": %s,\n' "$BUILD_SECONDS"
    printf '  "preparations": ['
    local first=1 preparation
    for preparation in "${PREPARE_RESULTS[@]+"${PREPARE_RESULTS[@]}"}"; do
      ((first)) || printf ', '
      printf '\n    {"command": %s, "exit_status": %s, "seconds": %s}' \
        "$(json_string "${preparation%%|*}")" \
        "$(echo "$preparation" | cut -d'|' -f2)" \
        "$(echo "$preparation" | cut -d'|' -f3)"
      first=0
    done
    ((first)) || printf '\n  '
    printf '],\n'
    printf '  "command": ['
    first=1
    local argument
    for argument in "${COMMAND[@]}"; do
      ((first)) || printf ', '
      printf '%s' "$(json_string "$argument")"
      first=0
    done
    printf '],\n'
    printf '  "started_at_utc": "%s",\n' "$STARTED_AT"
    printf '  "finished_at_utc": "%s",\n' "$finished_at"
    printf '  "elapsed_seconds": %s,\n' "$elapsed"
    printf '  "exit_status": %s,\n' "$GATE_STATUS"
    printf '  "stdout_log": %s,\n' "$(json_file "$WORK_DIR/stdout.log")"
    printf '  "stderr_log": %s,\n' "$(json_file "$WORK_DIR/stderr.log")"
    printf '  "artefacts": ['
    first=1
    local artefact durable
    # Enumerated from the work directory, which is what the run actually wrote,
    # and reported at the durable copy's path. Enumerating the copies instead
    # would let a copy that never happened go unmentioned rather than counted.
    while IFS= read -r artefact; do
      [[ -z "$artefact" ]] && continue
      durable="$(durable_copy_of "$artefact")"
      ((first)) || printf ', '
      printf '\n    {"path": %s, "origin": %s, "sha256": %s, "bytes": %s}' \
        "$(json_string "$durable")" "$(json_string "$artefact")" \
        "$(json_digest "$durable")" "$(json_bytes "$durable")"
      first=0
    done < <(find "$ARTEFACTS" -type f 2>/dev/null | LC_ALL=C sort)
    ((first)) || printf '\n  '
    printf ']\n'
    printf '}\n'
  } >"$RECEIPT.partial"
  # Renamed only once it is whole: a receipt half-written by an interrupted
  # trap is the one artefact that must never be readable as a measurement.
  mv "$RECEIPT.partial" "$RECEIPT"
  chmod 600 "$RECEIPT"
  echo "receipt: $RECEIPT"
  if $EVIDENCE_DURABLE; then
    echo "evidence: $EVIDENCE_DIR"
  else
    echo "EVIDENCE IS NOT DURABLE: $EVIDENCE_FAULT"
  fi
  echo "$warning"
}

json_string() {
  COMMIT_JSON_INPUT="$1" python3 -c '
import json
import os
print(json.dumps(os.environ["COMMIT_JSON_INPUT"]))
'
}

json_file() {
  local durable
  durable="$(durable_copy_of "$1")"
  if [[ -f "$durable" ]]; then
    printf '{"path": %s, "origin": %s, "sha256": %s, "bytes": %s}' \
      "$(json_string "$durable")" "$(json_string "$1")" \
      "$(json_digest "$durable")" "$(json_bytes "$durable")"
  else
    printf 'null'
  fi
}

finalize() {
  local status=$?
  trap - EXIT INT TERM
  set +e
  # Reap before removing: a gate still holding files open in a checkout being
  # deleted is how an interrupted run leaves both a half-worktree and a process
  # nobody is waiting for. The driver's own cells are its business -- it starts
  # each in a new session so it can reap them -- but this child is ours.
  if [[ -n "${GATE_PID:-}" ]] && kill -0 "$GATE_PID" 2>/dev/null; then
    echo "reaping the gate command ($GATE_PID)" >&2
    kill -TERM "$GATE_PID" 2>/dev/null
    local waited=0
    while kill -0 "$GATE_PID" 2>/dev/null && ((waited < 20)); do
      sleep 1
      waited=$((waited + 1))
    done
    kill -KILL "$GATE_PID" 2>/dev/null
  fi
  if ((KEEP)); then
    echo "worktree kept at $WORKTREE" >&2
  elif [[ -d "$WORKTREE" ]]; then
    # `--force` because the gate writes its own `target/` into the worktree,
    # exactly as it does in the main tree. Removing the checkout is what keeps
    # the next residue audit and the next `git worktree list` honest.
    if git -C "$REPO_DIR" worktree remove --force "$WORKTREE" 2>/dev/null; then
      WORKTREE_REMOVED=true
    else
      rm -rf "$WORKTREE"
      git -C "$REPO_DIR" worktree prune >/dev/null 2>&1 || true
      [[ -d "$WORKTREE" ]] || WORKTREE_REMOVED=true
    fi
  fi
  git -C "$REPO_DIR" worktree prune >/dev/null 2>&1 || true
  publish_evidence
  write_receipt
  exit "$status"
}
# INT and TERM as well as EXIT: bash does not run an EXIT trap for an uncaught
# TERM, so without these a killed run leaves a checkout registered in the
# repository's worktree list and no receipt saying what happened.
trap finalize EXIT INT TERM

echo "pinned gate: $LABEL"
echo "describes_commit=$COMMIT_SHA"
echo "commit_subject=$COMMIT_SUBJECT"
echo "head_at_start=$HEAD_AT_START dirty=$DIRTY_AT_START"
echo "worktree=$WORKTREE"
echo "receipt=$RECEIPT"

git -C "$REPO_DIR" worktree add --detach "$WORKTREE" "$COMMIT_SHA" >&2

# The two preconditions the manifest declares and no cell performs: the frozen
# release directory, and `clients/typescript/node_modules` from the locked
# `npm ci`. Both are out-of-band in the main tree too -- the difference is that
# a fresh checkout has neither, and MEASURED here: without the npm one, four
# `gate_a` typescript cells go red for a reason that is about the checkout and
# not about the commit. A runner that produced those four reds would be worse
# than useless, because it would report them as the commit's.
PREPARE_RESULTS=()
run_preparation() {
  local label=$1 status started
  shift
  echo "preparing: $label" >&2
  started="$(date -u +%s)"
  set +e
  (cd "$WORKTREE" && "$@") >>"$WORK_DIR/stdout.log" 2>>"$WORK_DIR/stderr.log"
  status=$?
  set -e
  PREPARE_RESULTS+=("$label|$status|$(($(date -u +%s) - started))")
  if ((status != 0)); then
    echo "preparation failed: $label ($status); see $WORK_DIR/stderr.log" >&2
    GATE_STATUS=2
    exit 2
  fi
}

if ((RELEASE_BUILD)); then
  build_start="$(date -u +%s)"
  run_preparation "cargo build --locked --release --workspace" \
    cargo build --locked --release --workspace
  BUILD_SECONDS=$(($(date -u +%s) - build_start))
  BUILD_STATUS=0
fi
for preparation in "${PREPARE[@]}"; do
  run_preparation "$preparation" bash -c "$preparation"
done

echo "running: ${COMMAND[*]}" >&2
set +e
(cd "$WORKTREE" && exec "${COMMAND[@]}") \
  >>"$WORK_DIR/stdout.log" 2>>"$WORK_DIR/stderr.log" &
GATE_PID=$!
wait "$GATE_PID"
GATE_STATUS=$?
set -e
GATE_PID=""
tail -n 20 "$WORK_DIR/stdout.log" >&2 || true
echo "gate exit_status=$GATE_STATUS" >&2
if ((GATE_STATUS != 0)); then
  exit 1
fi
