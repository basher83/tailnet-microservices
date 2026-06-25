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

- **`YYYY-MM-DD`** — anchored to the run's `generated:` timestamp (creation date); dirs sort chronologically.
- **`slug`** — kebab-case of the goal, for human scanning.
- **`goalId`** — stable unique key; exact-match searchable (`grep -r ud-j6rdih`).

## Runs

| Date | Goal | Phase | goalId | Generated (UTC) |
|------|------|-------|--------|-----------------|
| 2026-05-09 | [Deployed proxy live-validation closeout](./2026-05-09_live-validation-closeout_ud-j6rdih/) | cleanup | `ud-j6rdih` | 2026-05-09T07:33:06.167Z |
| 2026-06-25 | [Q25 — OAuth-Proxy Request-Parameter Span Capture (distilled)](./2026-06-25_q25-oauth-proxy-request-parameter-span-capture-distilled_ud-dzu6t0/) | cleanup | `ud-dzu6t0` | 2026-06-25T06:04:30.120Z |
