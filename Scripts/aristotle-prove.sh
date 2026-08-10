#!/usr/bin/env bash
#
# aristotle-prove.sh - hand one or more Lean files to Aristotle (Harmonic's Lean
# 4 prover, https://aristotle.harmonic.fun) and print the proofs it comes back
# with, as a unified diff against what you sent.
#
# It never edits your working tree. That's deliberate: the caller (a Claude
# task, or physlib-auto-task.sh's --prover aristotle path) reads the diff and
# decides what to splice in, rather than having files rewritten underneath it.
#
# Usage:
#   aristotle-prove.sh FILE.lean [FILE.lean ...] [--prompt TEXT] [--repo-root DIR]
#
#   --prompt TEXT     What to ask Aristotle for. Default: fill in every sorry.
#   --repo-root DIR   Root of the Lean project the files belong to; used to find
#                     lean-toolchain / lakefile / lake-manifest.json to send as
#                     context, and to label paths in the diff. Default: the
#                     nearest ancestor of the first file containing a lakefile.
#   --timeout SECS    Give up waiting after SECS (default 7200 - Aristotle runs
#                     are slow; a published case study reports ~8 hours on a
#                     hard problem). On timeout the remote task is cancelled.
#   --apply           Also WRITE the returned files over the originals. Off by
#                     default. Intended for the unattended --prover aristotle
#                     path, where there's no agent to read the diff and splice
#                     the proof in by hand; the build afterwards is what
#                     actually vets the result.
#
# Requires: ARISTOTLE_API_KEY (Dashboard -> API Keys at aristotle.harmonic.fun)
# and uv/uvx (the CLI is run with `uvx --from aristotlelib@latest aristotle`, so
# there's nothing to install first).
#
# CLI surface this was written against, verified with `aristotle --help` on
# aristotlelib as of 2026-07:
#   aristotle submit PROMPT [--project-dir DIR] [--wait] [--destination PATH]
#   aristotle download PROJECT_ID [--destination PATH]
#   aristotle show PROJECT_ID [--task ID] [--limit N]
#   aristotle cancel [PROJECT_OR_TASK_ID] [--project-id ID] [--task-id ID]
#   aristotle continue PROJECT_ID PROMPT [--mode ask|instruct] [--files ...]
#   Auth: --api-key, or the ARISTOTLE_API_KEY env var.
# `--project-dir` uploads the WHOLE directory, so we stage a minimal one below
# rather than pointing it at a Lean checkout (a built Physlib has a multi-GB
# .lake/ in it).

set -euo pipefail

SELF_NAME="$(basename "$0")"

die() { printf '%s: %s\n' "$SELF_NAME" "$*" >&2; exit 1; }
log() { printf '  -> %s\n' "$*" >&2; }

FILES=()
PROMPT=""
REPO_ROOT=""
TIMEOUT=7200
APPLY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prompt)      shift; [ $# -gt 0 ] || die "--prompt needs text"; PROMPT="$1" ;;
    --prompt=*)    PROMPT="${1#*=}" ;;
    --repo-root)   shift; [ $# -gt 0 ] || die "--repo-root needs a directory"; REPO_ROOT="$1" ;;
    --repo-root=*) REPO_ROOT="${1#*=}" ;;
    --timeout)     shift; [ $# -gt 0 ] || die "--timeout needs a number"; TIMEOUT="$1" ;;
    --timeout=*)   TIMEOUT="${1#*=}" ;;
    --apply)       APPLY=1 ;;
    -h|--help)     sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*)            die "Unknown flag: $1" ;;
    *)             FILES+=("$1") ;;
  esac
  shift
done

[ "${#FILES[@]}" -gt 0 ] || die "no Lean files given. Usage: $SELF_NAME FILE.lean [...] [--prompt TEXT]"
[ -n "${ARISTOTLE_API_KEY:-}" ] || die "ARISTOTLE_API_KEY is not set.
Get a key at https://aristotle.harmonic.fun (Dashboard -> API Keys), then:
  export ARISTOTLE_API_KEY='...'"

if command -v uvx >/dev/null 2>&1; then
  ARISTOTLE=(uvx --from aristotlelib@latest aristotle)
elif command -v uv >/dev/null 2>&1; then
  ARISTOTLE=(uv tool run --from aristotlelib@latest aristotle)
else
  die "uv/uvx not found - needed to run the Aristotle CLI. See https://astral.sh/uv"
fi

for f in "${FILES[@]}"; do
  [ -f "$f" ] || die "not a file: $f"
done

# Work out the project root, so we can send the small files that tell Aristotle
# which Lean/Mathlib it's targeting.
if [ -z "$REPO_ROOT" ]; then
  REPO_ROOT="$(cd "$(dirname "${FILES[0]}")" && pwd)"
  while [ "$REPO_ROOT" != "/" ]; do
    if [ -f "$REPO_ROOT/lakefile.lean" ] || [ -f "$REPO_ROOT/lakefile.toml" ]; then break; fi
    REPO_ROOT="$(dirname "$REPO_ROOT")"
  done
  [ "$REPO_ROOT" = "/" ] && REPO_ROOT="$(cd "$(dirname "${FILES[0]}")" && pwd)"
fi

[ -n "$PROMPT" ] || PROMPT="Fill in the sorries in the Lean files listed below. Keep every theorem, lemma and definition STATEMENT byte-for-byte identical - change only the proofs. Do not add new declarations or change imports. Return the complete files."

# --- stage the project source ----------------------------------------------
# The whole source tree, minus build output and VCS metadata. An earlier
# version sent only the target files plus lakefile/toolchain, which was wrong
# in practice: Aristotle landed in a project with no `Physlib/` directory,
# couldn't resolve imports or find the modules it had been asked about, and
# gave up asking where the source had gone. The source tree is small (~11 MB
# for Physlib); it's `.lake/` that's enormous, and that's what we exclude.
STAGE="$(mktemp -d)"
RESULT_ARCHIVE="$(mktemp -u)".tar.gz
EXTRACT_DIR="$(mktemp -d)"
PROJECT_ID=""
FINISHED=0

cleanup() {
  local rc=$?
  # If we're bailing out (interrupt, timeout, error) while a remote task is
  # still running, cancel it rather than leaving it burning credits.
  if [ -n "$PROJECT_ID" ] && [ "$FINISHED" -eq 0 ]; then
    log "Cancelling Aristotle project $PROJECT_ID"
    "${ARISTOTLE[@]}" cancel --project-id "$PROJECT_ID" >/dev/null 2>&1 || true
  fi
  rm -rf "$STAGE" "$EXTRACT_DIR" "$RESULT_ARCHIVE" 2>/dev/null || true
  return $rc
}
trap cleanup EXIT INT TERM

# Copy the tree, preserving structure so imports still resolve on the far side.
( cd "$REPO_ROOT" && tar cf - \
    --exclude='./.lake' --exclude='./.git' --exclude='./target' \
    --exclude='*.olean' --exclude='*.ilean' --exclude='*.trace' . ) \
  | ( cd "$STAGE" && tar xf - ) \
  || die "Couldn't stage the project from $REPO_ROOT"

# Record each target as a path RELATIVE to the repo root - that's how it's laid
# out in the upload, and how we find it again in the result archive.
REL_PATHS=()
for f in "${FILES[@]}"; do
  abs="$(cd "$(dirname "$f")" && pwd)/$(basename "$f")"
  rel="${abs#"$REPO_ROOT"/}"
  [ "$rel" = "$abs" ] && die "$f is not inside --repo-root ($REPO_ROOT)"
  [ -f "$STAGE/$rel" ] || die "$rel didn't make it into the upload (is it excluded?)"
  REL_PATHS+=("$rel")
done

# Scope the request to the files we actually chose. The whole project is
# uploaded so imports resolve, but that makes an unscoped "fill in every sorry"
# mean all ~690 files - so the targets are always spelled out. Also head off
# the interactive question mode: the CLI will prompt on stdin and we're
# non-interactive, which shows up as "Error answering question: EOF".
PROMPT="$PROMPT

Work ONLY on these files (paths relative to the project root):
$(printf '  - %s\n' "${REL_PATHS[@]}")
Leave every other file in the project untouched - they are provided only so
imports resolve and you can read context.

Do NOT ask me any questions: this is a non-interactive run and nobody can
answer. If something you need is missing or a goal turns out to be false, make
the best progress you can and say so in your final message."

STAGED_SIZE="$(du -sh "$STAGE" 2>/dev/null | awk '{print $1}')"
log "Staged the project from $REPO_ROOT (${STAGED_SIZE:-unknown}, .lake excluded)"
log "Asking Aristotle to work on: ${REL_PATHS[*]}"
log "This can take a long time (hours, on a hard goal). Progress follows."

# --- submit ----------------------------------------------------------------
# The CLI's output is teed so it streams live AND can be scraped afterwards for
# the project id (needed to cancel on interrupt, and to point you at the run).
# Note the process substitution rather than a `| tee` pipeline: backgrounding a
# pipeline makes `$!`/`wait` report tee's exit status, which would silently
# swallow a failed submit.
SUBMIT_LOG="$(mktemp)"
set +e
"${ARISTOTLE[@]}" submit "$PROMPT" \
  --project-dir "$STAGE" \
  --wait \
  --destination "$RESULT_ARCHIVE" > >(tee "$SUBMIT_LOG" >&2) 2>&1 &
SUBMIT_PID=$!

# Watchdog: a run that never returns shouldn't hang the harness forever. macOS
# has no `timeout(1)`, so this is done by hand.
( waited=0
  while [ "$waited" -lt "$TIMEOUT" ]; do
    kill -0 "$SUBMIT_PID" 2>/dev/null || exit 0
    sleep 5; waited=$((waited + 5))
  done
  log "Timed out after ${TIMEOUT}s - stopping."
  kill "$SUBMIT_PID" 2>/dev/null ) &
WATCHDOG_PID=$!

wait "$SUBMIT_PID"
SUBMIT_RC=$?
set -e
kill "$WATCHDOG_PID" 2>/dev/null || true
sleep 1   # let tee flush before we read the log back

PROJECT_ID="$(grep -oE '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}' "$SUBMIT_LOG" | head -1 || true)"
rm -f "$SUBMIT_LOG"
[ -n "$PROJECT_ID" ] && log "Aristotle project: $PROJECT_ID"

[ "$SUBMIT_RC" -eq 0 ] || die "Aristotle submit failed (exit $SUBMIT_RC).${PROJECT_ID:+ Inspect it with: aristotle show $PROJECT_ID}"
# Finished cleanly, so there's no longer a running task for cleanup to cancel -
# but keep the id itself, the messages below still reference it.
FINISHED=1

# --- unpack and diff -------------------------------------------------------
[ -s "$RESULT_ARCHIVE" ] || die "Aristotle returned no result archive.${PROJECT_ID:+ Check: aristotle show $PROJECT_ID}"

if ! tar -xzf "$RESULT_ARCHIVE" -C "$EXTRACT_DIR" 2>/dev/null; then
  # Be tolerant about the archive format rather than assuming gzip-tar.
  if command -v unzip >/dev/null 2>&1 && unzip -qq -o "$RESULT_ARCHIVE" -d "$EXTRACT_DIR" 2>/dev/null; then
    :
  else
    die "Couldn't unpack the result archive ($RESULT_ARCHIVE)."
  fi
fi

printf '\n===== Aristotle result =====\n'
found=0
CHANGED=0
i=0
for f in "${FILES[@]}"; do
  rel="${REL_PATHS[$i]}"
  i=$((i + 1))
  # The archive may nest the project under a wrapper directory, so match on the
  # relative path rather than a bare basename - Physlib has many same-named
  # files (Basic.lean and friends) and a basename match picks the wrong one.
  returned=""
  if [ -f "$EXTRACT_DIR/$rel" ]; then
    returned="$EXTRACT_DIR/$rel"
  else
    returned="$(find "$EXTRACT_DIR" -type f -path "*/$rel" -print -quit 2>/dev/null || true)"
  fi
  if [ -z "$returned" ]; then
    printf '\n--- %s: no file at that path came back ---\n' "$rel"
    continue
  fi
  found=1
  if diff -q "$f" "$returned" >/dev/null 2>&1; then
    printf '\n--- %s: unchanged ---\n' "$rel"
  else
    printf '\n--- %s: Aristotle changed this file ---\n' "$rel"
    diff -u --label "a/$rel" --label "b/$rel" "$f" "$returned" || true
    if [ "$APPLY" -eq 1 ]; then
      cp "$returned" "$f"
      CHANGED=$((CHANGED + 1))
      printf '(applied to %s)\n' "$rel"
    fi
  fi
done

if [ "$found" -eq 0 ]; then
  printf '\nNothing recognisable came back. Files in the archive:\n'
  find "$EXTRACT_DIR" -type f -printf '  %P\n' 2>/dev/null || find "$EXTRACT_DIR" -type f
  exit 1
fi

printf '\n===== end of Aristotle result =====\n'
if [ "$APPLY" -eq 1 ]; then
  printf '%d file(s) written. Build before trusting any of it.\n' "$CHANGED"
  # Nothing came back changed: the caller has nothing to build or commit, and
  # should hear about it as a failure rather than an empty success.
  [ "$CHANGED" -gt 0 ] || exit 2
else
  printf 'Nothing has been written to your working tree - apply what you want by hand.\n'
fi
