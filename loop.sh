#!/bin/bash
# Ralph Loop for Tailnet Microservices
# Usage: ./loop.sh [--json] [plan|plan-work "scope"|max_iterations]
# Examples:
#   ./loop.sh                          # Build mode, human output
#   ./loop.sh --json                   # Build mode, JSON output
#   ./loop.sh 20                       # Build mode, max 20 tasks
#   ./loop.sh plan                     # Plan mode, unlimited
#   ./loop.sh plan 5                   # Plan mode, max 5 iterations
#   ./loop.sh plan-work "token refresh" # Scoped plan for work branch
#   ./loop.sh --json plan              # Plan mode, JSON output

# --- Sandbox pre-flight ---
check_sandbox() {
    if [ -f /.dockerenv ] || [ -f /run/.containerenv ] || grep -qsE 'docker|lxc|kubepods' /proc/1/cgroup 2>/dev/null; then
        echo "✅ Sandbox: container detected"
    elif command -v bwrap &>/dev/null; then
        echo "✅ Sandbox: bubblewrap available"
    elif command -v sandbox-exec &>/dev/null; then
        echo "✅ Sandbox: macOS Seatbelt available"
    else
        echo "⚠️  No sandbox detected — running uncontained"
        echo "   Consider: Docker, bubblewrap, or macOS sandbox-exec"
    fi
}

# Parse --json flag
OUTPUT_FORMAT=""
if [ "$1" = "--json" ]; then
    OUTPUT_FORMAT="--output-format=stream-json"
    shift
fi

# Parse mode and iterations
if [ "$1" = "plan-work" ]; then
    MODE="plan-work"
    PROMPT_FILE="PROMPT_plan_work.md"
    if [ -z "$2" ]; then
        echo "Error: plan-work requires a scope description"
        echo "Usage: ./loop.sh plan-work \"token refresh logic\""
        exit 1
    fi
    export WORK_SCOPE="$2"
    MAX_ITERATIONS=${3:-5}
    # Branch validation — plan-work must not run on main/master
    CURRENT_BRANCH=$(git branch --show-current)
    if [ "$CURRENT_BRANCH" = "main" ] || [ "$CURRENT_BRANCH" = "master" ]; then
        echo "Error: plan-work must run on a work branch, not $CURRENT_BRANCH"
        echo "Create one: git checkout -b work/$(echo "$2" | tr ' ' '-' | tr '[:upper:]' '[:lower:]')"
        exit 1
    fi
elif [ "$1" = "plan" ]; then
    MODE="plan"
    PROMPT_FILE="PROMPT_plan.md"
    MAX_ITERATIONS=${2:-0}
elif [[ "$1" =~ ^[0-9]+$ ]]; then
    MODE="build"
    PROMPT_FILE="PROMPT_build.md"
    MAX_ITERATIONS=$1
else
    MODE="build"
    PROMPT_FILE="PROMPT_build.md"
    MAX_ITERATIONS=0
fi

ITERATION=0
CURRENT_BRANCH=$(git branch --show-current)
CLAUDE_PID=""

# Signal handler - kill claude process and exit
cleanup() {
    echo -e "\n\n⚠️  Caught signal, stopping..."
    if [ -n "$CLAUDE_PID" ] && kill -0 "$CLAUDE_PID" 2>/dev/null; then
        kill -TERM "$CLAUDE_PID" 2>/dev/null
        sleep 0.5
        kill -9 "$CLAUDE_PID" 2>/dev/null
    fi
    exit 130
}
trap cleanup SIGINT SIGTERM SIGQUIT

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🦀 Tailnet Microservices — Ralph Loop"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Mode:   $MODE"
echo "Prompt: $PROMPT_FILE"
echo "Output: ${OUTPUT_FORMAT:-human}"
echo "Branch: $CURRENT_BRANCH"
[ "$MODE" = "plan-work" ] && echo "Scope:  $WORK_SCOPE"
[ $MAX_ITERATIONS -gt 0 ] && echo "Max:    $MAX_ITERATIONS iterations"
echo "Stop:   Ctrl+C"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

check_sandbox

# Verify prompt file exists
if [ ! -f "$PROMPT_FILE" ]; then
    echo "Error: $PROMPT_FILE not found"
    exit 1
fi

while true; do
    if [ $MAX_ITERATIONS -gt 0 ] && [ $ITERATION -ge $MAX_ITERATIONS ]; then
        if [ "$MODE" = "plan-work" ]; then
            echo "✅ Scoped planning complete ($MAX_ITERATIONS iterations)"
            echo "   Next: review IMPLEMENTATION_PLAN.md, then ./loop.sh to build"
        else
            echo "Reached max iterations: $MAX_ITERATIONS"
        fi
        break
    fi

    # Prepare prompt — envsubst for plan-work, cat for others
    if [ "$MODE" = "plan-work" ]; then
        envsubst '${WORK_SCOPE}' < "$PROMPT_FILE" | claude -p \
            --dangerously-skip-permissions \
            $OUTPUT_FORMAT \
            --model opus \
            --verbose &
    else
        cat "$PROMPT_FILE" | claude -p \
            --dangerously-skip-permissions \
            $OUTPUT_FORMAT \
            --model opus \
            --verbose &
    fi
    CLAUDE_PID=$!
    wait $CLAUDE_PID
    CLAUDE_PID=""

    # Push changes after each iteration
    git push origin "$CURRENT_BRANCH" 2>/dev/null || {
        echo "Creating remote branch..."
        git push -u origin "$CURRENT_BRANCH"
    }

    # Convergence detection (build mode only)
    if [ "$MODE" = "build" ] && [ -f ".claude/hooks/convergence-check.sh" ]; then
        if ! bash .claude/hooks/convergence-check.sh; then
            echo "Loop auto-terminated: convergence detected after $((ITERATION + 1)) iterations"
            break
        fi
    fi

    ITERATION=$((ITERATION + 1))
    echo -e "\n\n════════════════════ LOOP $ITERATION ════════════════════\n"
done
