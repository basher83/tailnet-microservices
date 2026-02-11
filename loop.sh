#!/bin/bash
# Ralph Loop for Tailnet Microservices
# Usage: ./loop.sh [--json] [plan|plan-work "scope"|task "description"|max_iterations]
# Examples:
#   ./loop.sh                          # Build mode, human output
#   ./loop.sh --json                   # Build mode, JSON output
#   ./loop.sh 20                       # Build mode, max 20 tasks
#   ./loop.sh plan                     # Plan mode, unlimited
#   ./loop.sh plan 5                   # Plan mode, max 5 iterations
#   ./loop.sh plan-work "token refresh" # Scoped plan for work branch
#   ./loop.sh task "implement specs/release-workflow.md"  # Task mode, scoped to one task
#   ./loop.sh task "fix PKCE verifier length" 3  # Task mode, max 3 iterations
#   ./loop.sh --json plan              # Plan mode, JSON output

# --- Sandbox pre-flight ---
check_sandbox() {
    if [ -f /.dockerenv ] || [ -f /run/.containerenv ] || grep -qsE 'docker|lxc|kubepods' /proc/1/cgroup 2>/dev/null; then
        echo "Sandbox: container detected"
    elif command -v bwrap &>/dev/null; then
        echo "Sandbox: bubblewrap available"
    elif command -v sandbox-exec &>/dev/null; then
        echo "Sandbox: macOS Seatbelt available"
    else
        echo "No sandbox detected — running uncontained"
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
if [ "$1" = "task" ]; then
    if [ -z "$2" ]; then
        echo "Error: task requires a description or spec path"
        echo "Usage: ./loop.sh task \"description of the task\" [max_iterations]"
        exit 1
    fi
    MODE="task"
    PROMPT_FILE="PROMPT_build.md"
    TASK_DESC="$2"
    MAX_ITERATIONS=${3:-0}
elif [ "$1" = "plan-work" ]; then
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
    echo -e "\n\nCaught signal, stopping..."
    if [ -n "$CLAUDE_PID" ] && kill -0 "$CLAUDE_PID" 2>/dev/null; then
        kill -TERM "$CLAUDE_PID" 2>/dev/null
        sleep 0.5
        kill -9 "$CLAUDE_PID" 2>/dev/null
    fi
    exit 130
}
trap cleanup SIGINT SIGTERM SIGQUIT

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Tailnet Microservices — Ralph Loop"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Mode:   $MODE"
echo "Prompt: $PROMPT_FILE"
echo "Output: ${OUTPUT_FORMAT:-human}"
echo "Branch: $CURRENT_BRANCH"
[ "$MODE" = "task" ] && echo "Task:   $TASK_DESC"
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
    # Task mode: exit when TASK.md has been deleted (agent signals completion)
    if [ "$MODE" = "task" ] && [ $ITERATION -gt 0 ] && [ ! -f TASK.md ]; then
        echo "Task complete — TASK.md deleted by agent."
        break
    fi

    # Create TASK.md at start (first iteration only)
    if [ "$MODE" = "task" ] && [ $ITERATION -eq 0 ]; then
        echo "$TASK_DESC" > TASK.md
        echo "Created TASK.md"
    fi

    if [ $MAX_ITERATIONS -gt 0 ] && [ $ITERATION -ge $MAX_ITERATIONS ]; then
        if [ "$MODE" = "plan-work" ]; then
            echo "Scoped planning complete ($MAX_ITERATIONS iterations)"
            echo "   Next: review IMPLEMENTATION_PLAN.md, then ./loop.sh to build"
        else
            echo "Reached max iterations: $MAX_ITERATIONS"
        fi
        if [ "$MODE" = "task" ] && [ -f TASK.md ]; then
            echo ""
            echo "TASK.md still exists — task may be incomplete."
        fi
        break
    fi

    # Prepare prompt — envsubst for plan-work, redirect for others
    if [ "$MODE" = "plan-work" ]; then
        envsubst '${WORK_SCOPE}' < "$PROMPT_FILE" | claude -p \
            --dangerously-skip-permissions \
            ${OUTPUT_FORMAT:+"$OUTPUT_FORMAT"} \
            --model opus \
            --verbose &
    else
        claude -p \
            --dangerously-skip-permissions \
            ${OUTPUT_FORMAT:+"$OUTPUT_FORMAT"} \
            --model opus \
            --verbose \
            < "$PROMPT_FILE" &
    fi
    CLAUDE_PID=$!
    wait $CLAUDE_PID
    EXIT_CODE=$?
    CLAUDE_PID=""

    if [ $EXIT_CODE -ne 0 ]; then
        echo "Claude exited with code $EXIT_CODE (iteration $((ITERATION + 1)))"
    fi

    # Push changes after each iteration
    if ! git push origin "$CURRENT_BRANCH" 2>/dev/null; then
        if ! git push -u origin "$CURRENT_BRANCH" 2>/dev/null; then
            echo "git push failed (iteration $((ITERATION + 1)))"
            echo "   Local commits accumulating — check auth/network."
        fi
    fi

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
