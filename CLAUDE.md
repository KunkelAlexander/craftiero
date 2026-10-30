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

# Project: craftiero (3D SDF worm shooter)

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
`sdf` (M0), `sim` (M2-M3: worms, weapons, combat, the match rules (score limit, winner), the map generator `sim::map`, pure, headless-tested), `protocol` and `host` (M4-M5: messages, the authority, the client `Replica`, the host `Session`), `server` (M5: the relay, and matches it hosts itself) and `viewer` (wgpu raymarcher, the game loop, effects, the menu, and the online client; becomes `game` later) are in the workspace. The viewer plays offline through `viewer/src/local.rs`: host + in-memory wire + replica, the same path as online; `HOST` adds a relay room to that (`share.rs`), `JOIN` and `SERVER ROOM` draw a friend's or the server's match through `join.rs`. `sim/old2d/` (the 2D bots) is kept for porting at M6; everything else from the 2D game was ported and deleted (git history has it). Online design: `docs/NETWORKING.md`; deployment: `docs/DEPLOY.md`.

## Commands
- `cargo test -p sdf -p sim -p protocol -p host -p viewer -p server`, `cargo run -p sdf --release --example bench`, `cargo run -p server --release --example tick_cost` (what a hosted match costs per tick), `cargo run -p server --release` (the relay on port 9001)
- `cargo run -p viewer --release` (interactive), `-- --shot out.png [--edits --cam x,y,z,yaw,pitch --worm x,z,yaw [--rope]]` (headless screenshot; `--fire id[,ticks]` fires that weapon and shoots the picture some ticks later, `--weapon id` holds it with no shot, `--no-brush` hides the dig ring, `--no-aim` hides the weapon aim aid (arcs, blast rings, ricochet marks), `--hud` draws the HUD (`--won` with it: a finished match), `--menu title|code|notice` draws a menu screen), `-- --bench` (GPU ms per pass). Windows defaults to DX12; `WGPU_BACKEND=vulkan` overrides.
- Browser (WebGPU only; online play needs a relay, see docs/DEPLOY.md): `powershell -File web/build.ps1 -Serve`, then open http://127.0.0.1:8080. Needs `rustup target add wasm32-unknown-unknown` and `cargo install wasm-bindgen-cli --version 0.2.129 --locked` (same version as Cargo.lock). See the web section of `docs/PERFORMANCE.md`. Phones get touch controls (`viewer/src/touch.rs`, pure and unit-tested, drawn by the HUD).
