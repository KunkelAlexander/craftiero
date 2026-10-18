# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.
---

# Project: 3D SDF Liero (working title)

Fork of the 2D "Neon Bomber-Liero" (see git history before the "Fork for 3D" commit for the 2D code). Real-time 3D worm shooter with fully destructible/formable terrain stored as a signed distance field. Rust, native first (wgpu + winit), web later. Cel shading with Okami-like ink outlines. Smooth, fast gameplay beats graphics. The owner wants to **learn**, so the code should teach. The full plan and milestones (M0-M6) are in `PLAN.md`.

## Code style
- Keep the code simple: plain structs, enums, functions. No generics, traits or macros without a clear payoff. No ECS.
- Comment the *why* next to the code. Name Game Programming Patterns where used (`// Pattern: Dirty Flag`...), and explain networking concepts where they appear.
- No external assets: procedural shapes, synthesized sound, GLSL/WGSL in source strings.

## Architecture rules (inherited from the 2D game)
- All game logic lives in `sim` (no window, GPU, I/O or clock), driven only by per-player `Input` structs. Rendering, audio and networking read state and events.
- Terrain lives in the pure `sdf` crate: a chunked distance grid. Collision samples it directly (no collision mesh). The renderer keeps a GPU copy of the grid (a 3D texture) and raymarches it, no meshes; only dirty chunks are re-uploaded. Edits (`Carve`/`Add` shapes) are the network event format, like `TileChanged` in 2D.
- "Host" is a role: whoever calls `World::step` is the authority; it must stay movable into a dedicated server. Clients draw a `Replica` built from snapshots, even offline (in-memory loopback).
- Fixed 60 Hz sim; render at display rate. Weapons are data rows (`WeaponDef`).

## Workspace state
`sdf` (M0), `sim` (M2: the worm, pure, headless-tested) and `viewer` (wgpu raymarcher plus the M2 game loop, becomes `game` later) are in the workspace. `sim/old2d/` (weapons, bots, the 2D world) is kept for porting at M3. `protocol`, `host`, `server`, `game/src/{share,local,join,net,socket}.rs` are the 2D code kept for porting; they do not compile yet. Re-add each to `members` at its milestone.

## Commands
- `cargo test -p sdf -p sim -p viewer`, `cargo run -p sdf --release --example bench`
- `cargo run -p viewer --release` (interactive), `-- --shot out.png [--edits --cam x,y,z,yaw,pitch --worm x,z,yaw [--rope]]` (headless screenshot), `-- --bench` (GPU ms per pass). Windows defaults to DX12; `WGPU_BACKEND=vulkan` overrides.
