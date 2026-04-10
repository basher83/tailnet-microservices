#!/bin/bash
# spec-frontmatter-guard.sh — PostToolUse hook for Write/Edit on specs/*.md
# Validates YAML frontmatter: status must be exactly Active|Complete|Superseded,
# created must be YYYY-MM-DD format. Rejects non-compliant writes.
set -euo pipefail

input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // ""')
file_path=$(echo "$input" | jq -r '.tool_input.file_path // ""')

# Only validate Write/Edit on specs/*.md (not README.md)
case "$tool_name" in
    Write|Edit) ;;
    *) exit 0 ;;
esac

[[ "$file_path" == */specs/*.md ]] || exit 0
[[ "$(basename "$file_path")" != "README.md" ]] || exit 0

# Verify file exists
[ -f "$file_path" ] || exit 0

deny() {
    echo "{\"decision\":\"block\",\"reason\":\"$1\"}"
    exit 2
}

# Check frontmatter starts on line 1 with ---
first_line=$(head -1 "$file_path")
if [ "$first_line" != "---" ]; then
    deny "Spec must begin with YAML frontmatter (first line must be ---). Required: status (Active|Complete|Superseded) and created (YYYY-MM-DD)."
fi

# Check closing --- exists within first 20 lines (after line 1)
closing=$(sed -n '2,20p' "$file_path" | grep -n '^---$' | head -1 | cut -d: -f1)
if [ -z "$closing" ]; then
    deny "Spec frontmatter missing closing --- delimiter. Required: status (Active|Complete|Superseded) and created (YYYY-MM-DD)."
fi

# Extract frontmatter between the delimiters
frontmatter=$(sed -n "2,$((closing))p" "$file_path")

# Validate status field exists and is exactly one of the closed enum
status=$(printf '%s\n' "$frontmatter" | grep -E '^status:' | sed 's/^status:[[:space:]]*//' | tr -d '[:space:]' || true)
if [ -z "$status" ]; then
    deny "Spec missing 'status' field in frontmatter. Required: Active, Complete, or Superseded."
fi

case "$status" in
    Active|Complete|Superseded) ;;
    *) deny "Invalid status '$status'. Must be exactly: Active, Complete, or Superseded. No qualifiers." ;;
esac

# Validate created field exists and matches YYYY-MM-DD
created=$(printf '%s\n' "$frontmatter" | grep -E '^created:' | sed 's/^created:[[:space:]]*//' | tr -d '[:space:]' || true)
if [ -z "$created" ]; then
    deny "Spec missing 'created' field in frontmatter. Required format: YYYY-MM-DD."
fi

if ! echo "$created" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
    deny "Invalid created date '$created'. Must be YYYY-MM-DD format."
fi

exit 0
