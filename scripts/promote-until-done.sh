#!/usr/bin/env bash
# Promote the current .until-done/ run into the durable, version-controlled
# docs/until-done/ archive. The .until-done/ working dir is gitignored; this
# snapshots distilled.md + tasks.yaml so completed runs survive the next loop.
#
# Convention: docs/until-done/<YYYY-MM-DD>_<slug>_<goalId>/
#   - YYYY-MM-DD : from tasks.yaml `generated:` (creation anchor; sorts by date)
#   - slug       : kebab of the distilled.md H1 title (human scan)
#   - goalId     : stable unique key (exact-match searchable)
#
# Idempotent: re-running for the same goalId refreshes the existing dir rather
# than creating a duplicate. Originals in .until-done/ are left untouched.
#
# Usage: mise run until-done:promote   (or: ./scripts/promote-until-done.sh)
set -euo pipefail

SRC_DIR=".until-done"
DEST_ROOT="docs/until-done"
TASKS="$SRC_DIR/tasks.yaml"
DISTILLED="$SRC_DIR/distilled.md"
INDEX="$DEST_ROOT/README.md"

[ -f "$TASKS" ] || { echo "FAIL: $TASKS not found (no active until-done run to promote)"; exit 1; }
[ -f "$DISTILLED" ] || { echo "FAIL: $DISTILLED not found"; exit 1; }

# --- Parse anchors from tasks.yaml (no yq dependency) ---
generated=$(grep -m1 '^generated:' "$TASKS" | awk '{print $2}')
goalId=$(grep -m1 '^goalId:' "$TASKS" | awk '{print $2}')
phase=$(grep -m1 '^phase:' "$TASKS" | awk '{print $2}')
date="${generated:0:10}"

[ -n "$generated" ] || { echo "FAIL: no 'generated:' field in $TASKS"; exit 1; }
[ -n "$goalId" ]    || { echo "FAIL: no 'goalId:' field in $TASKS"; exit 1; }
[ -n "$date" ]      || { echo "FAIL: could not derive date from generated='$generated'"; exit 1; }

# --- Human title + slug from distilled.md H1 ---
title=$(grep -m1 '^# ' "$DISTILLED" | sed 's/^# //')
[ -n "$title" ] || title="$goalId"
slug=$(printf '%s' "$title" \
  | tr '[:upper:]' '[:lower:]' \
  | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//')

# --- Resolve target dir: reuse existing *_<goalId> if present (idempotent) ---
existing=$(find "$DEST_ROOT" -maxdepth 1 -type d -name "*_${goalId}" 2>/dev/null | head -1 || true)
if [ -n "$existing" ]; then
  dest="$existing"
  echo "Reusing existing run dir: $dest"
else
  dest="$DEST_ROOT/${date}_${slug}_${goalId}"
  echo "Creating run dir: $dest"
fi

mkdir -p "$dest"
cp "$DISTILLED" "$TASKS" "$dest/"

# --- Ensure an index row exists (table lives at end of README) ---
mkdir -p "$DEST_ROOT"
if [ ! -f "$INDEX" ]; then
  cat > "$INDEX" <<'EOF'
# until-done archive

Durable, version-controlled copies of completed `/until-done` runs, promoted out
of the gitignored `.until-done/` working directory.

## Layout

```
docs/until-done/
  <YYYY-MM-DD>_<slug>_<goalId>/
    distilled.md   # PRD-shaped journey summary
    tasks.yaml     # the locked plan + per-task status/learnings
```

## Runs

| Date | Goal | Phase | goalId | Generated (UTC) |
|------|------|-------|--------|-----------------|
EOF
fi

if grep -q "$goalId" "$INDEX"; then
  echo "Index row for $goalId already present; left as-is."
else
  rel="${dest#"$DEST_ROOT/"}"
  printf '| %s | [%s](./%s/) | %s | `%s` | %s |\n' \
    "$date" "$title" "$rel" "${phase:-?}" "$goalId" "$generated" >> "$INDEX"
  echo "Appended index row for $goalId."
fi

echo "OK: promoted run $goalId -> $dest"
echo "Next: git add $DEST_ROOT/"
