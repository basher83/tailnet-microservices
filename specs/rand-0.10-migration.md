# Migrate rand crate to 0.10

## Context

Renovate PR #3 bumps `rand` from 0.9 to 0.10 in `Cargo.toml`. rand 0.10 contains breaking API renames that require code changes before the version bump compiles.

## Scope

Update all rand API call sites to be compatible with rand 0.10, regenerate `Cargo.lock`, and verify the workspace builds and passes tests.

## Breaking changes in rand 0.10

Source: [rand 0.10.0 changelog](https://github.com/rust-random/rand/blob/main/CHANGELOG.md)

| 0.9 | 0.10 | Notes |
|-----|------|-------|
| `rand::Rng` (extension trait) | `rand::RngExt` | Base trait `rand_core::RngCore` renamed to `rand_core::Rng`; old extension trait renamed to `RngExt` |
| `rand_chacha` dependency | `chacha20` crate | Internal change; CSPRNG output unchanged |
| `OsRng` | `SysRng` | Renamed |
| `OsError` | `SysError` | Renamed |
| `choose_multiple` | `sample` | Method renamed |
| `SeedableRng::from_os_rng` | Removed | Use `SysRng` instead |

## Affected code

Single file: `crates/anthropic-auth/src/pkce.rs`

- Line 11: `use rand::Rng;` — must become `use rand::RngExt;`
- Line 23: `rand::rng().fill(&mut bytes);` — `.fill()` moves from `Rng` to `RngExt`; call site unchanged once import is fixed

No other crate in the workspace imports `rand` directly.

## Acceptance criteria

1. `Cargo.toml` workspace dependency reads `rand = "0.10"`
2. `Cargo.lock` reflects resolved rand 0.10.x
3. `cargo build --workspace` succeeds
4. `cargo test --workspace` passes (including existing PKCE tests)
5. `cargo clippy --workspace -- -D warnings` clean
6. No use of deprecated rand 0.9 API names anywhere in `crates/` or `services/`
