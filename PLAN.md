# 3D SDF Liero: plan (the 2D plan is in docs/PLAN_2D.md)

## Context
Goal: a 3D, Liero-style worm shooter with fully destructible/formable terrain based on signed distance fields (SDFs), cel-shaded with Okami-like ink outlines, smooth fast gameplay first, possible online multiplayer, Rust-native, structured like this repo. Prior attempt (voxellini, marching cubes in Godot) was too slow.
Decisions from the user: **native first, web later**; **full 3D volume, small arena** (caves/tunnels, ~128x64x128 m at ~0.25 m voxels); **raymarch the SDF, do not mesh it** (decided at M1, see Status).

## Status (read this first)
- **Done:** fork + prune of the 2D repo (history kept), `CLAUDE.md` rewritten for 3D, **M0** the `sdf` crate, **M1** the `viewer` tech demo. **M1 is built and measured, and waits for the owner's review** (art style, feel, speed).
- **Workspace:** `sdf` and `viewer` are `members`. `sim`, `protocol`, `host`, `server`, `game/src/{share,local,join,net,socket}.rs` are 2D code kept for porting and do not compile yet (sim lost `tilemap.rs`). Re-add each at its milestone.
- **Not committed:** nothing from the fork, `sdf` or `viewer` is committed. `git status` shows the staged 2D deletions, the new crates, this file, `CLAUDE.md` and `screenshots/M1_*.png`. Review, then commit.
- **Tests:** `cargo test -p sdf -p viewer` (10 + 7 headless tests, all pass).

### Run it
```
cargo run -p viewer --release
```
Click the window to capture the mouse, Esc releases it (again to quit).

| Input | Does |
|---|---|
| W A S D, Space / Ctrl, Shift | fly, up / down, fast |
| left mouse (hold) | carve a sphere at the crosshair, repeats at 20 Hz |
| right mouse (hold) | build: a sphere pushed out along the wall, blended in like clay |
| wheel | brush radius (0.5 to 8 m) |
| 1 / 2 / 3 | fixed render resolution 100% / 75% / 50% |
| T | automatic resolution (default): holds ~60 fps by scaling it |
| G / H / O / V | shadows / ambient occlusion / ink outlines / vsync |
| R | regenerate the map (about 0.6 s) |

The title bar shows fps, GPU ms (march + post), edit and upload cost, brush size and render scale. Headless modes: `-- --shot out.png [--edits] [--cam x,y,z,yaw,pitch] [--scale 0.5] [--no-shadows --no-ao --no-outlines]` and `-- --bench [--size 1920x1080]`.

### M1 decision: raymarch the grid, do not mesh it
The first plan drew the terrain with Surface Nets meshes. The owner doubted the look, and research on the video's author (Mike Turitzin, a custom SDF engine inspired by Dreams and Claybook) found he **raymarches baked SDF bricks** (WebGPU, clipmap LOD, trilinear surfaces). So M1 raymarches. What I could and could not confirm: his X posts and the 80.lv article say this; the video itself (title only) and its transcript were not readable. The CPU side did not change: `sdf::Grid` is still the source of truth for collision and edits. `sdf::mesh` and `fast-surface-nets` still exist and are not used by the viewer (a fallback if raymarching fails on some GPU).

### How the viewer works (read `main.rs`, `renderer.rs`, `shaders.rs` in that order)
1. **Terrain on the GPU.** A copy of the grid lives in a 512 x 256 x 512 `R8Snorm` 3D texture (64 MB). The sampler's trilinear filter does the interpolation `Grid::sample` does, which is what makes the surface smooth. After an edit only the changed chunks are re-uploaded, as ONE `write_texture` over their bounding box.
2. **Pass 1, march** (fragment shader, one triangle): sphere-trace each pixel, then shade: toon ramp (3 flat bands), hard shadow ray, 4-tap ambient occlusion, warm headlamp in shadow, brush tint. Writes colour and a G-buffer (normal, distance).
3. **Pass 2, ink** at the same internal resolution: outlines from the G-buffer, not the colours. Depth silhouettes use the second difference of depth (flat slanted walls stay clean) and only the nearer side draws, creases come from normal differences. Lines wobble and vary in width with noise.
4. **Pass 3, present** at screen resolution: bilinear upscale, paper grain, vignette, crosshair.
5. **Automatic resolution:** the GPU times each pass with timestamp queries and the render scale follows (cost ~ scale squared, target 12 ms, steps of 5%, dead band +-8%).

### Optimisations: what mattered, and what did not
Measured on the dev laptop (below). "Before" and "after" were taken minutes apart; see the noise warning.
| Change | Effect |
|---|---|
| **Empty-space skipping**: a 32 x 16 x 32 byte map of "chunks to the nearest surface chunk" lets a ray leap `(k-1)` chunks at once (`coarse_map`, tested against brute force) | the reason a 128 m view is affordable at all |
| **Hit tolerance = half a pixel** (`eps + t * slope`) and a step floor equal to it | march 25 -> 12 ms at 720p; the biggest single win |
| **Over-relaxed steps** (1.4 x the safe distance; overshoot is repaired by one extra step back) | another 13-40% of the march (most on grazing views), no visible artefacts |
| **One `write_texture` per edit** (bounding box) instead of one per chunk | upload 23 ms -> 2-3 ms per crater |
| Skip the empty-space map rebuild when no chunk changed its surface status | digging in place costs ~1 ms less |
| **Ink pass at internal resolution**, upscale in a third pass | the render scale now also cuts the outline cost (it was 8.6 ms at a 1080p window) |
| **Map generation**: parallel chunks (`std::thread::scope`), a 4x4x4 block shortcut, a height-field lookup table, uniform chunks collapsed | 19.5 s -> 0.6 s |
| Tried and dropped: stored band 4 -> 8 voxels | no rendering gain, CPU edit cost doubled |
| Tried and dropped: shadow rays capped at 48 steps | leaves light slivers in long shadows; 128 is enough |

### Results
Intel UHD (laptop iGPU), DX12, 1280 x 720, render scale 100%, GPU ms from timestamp queries, best of 3 batches of 15 frames (`--bench`):
| View | everything | no shadows | march only |
|---|---|---|---|
| open hills (spawn view) | 18.6 | 11.7 | 9.5 |
| cavern | 18.0 | 13.7 | 11.6 |
| ground level, grazing rays | 23.2 | 16.7 | 16.5 |

- Edit: CPU `Grid::apply` for an r = 3 m crater **1.0 to 1.7 ms** (this is the M0 concern, now the only sim-thread cost); upload 2 to 4 ms of main-thread time.
- In the window: **60 fps at 65-70% render scale** with everything on (automatic mode), on this GPU. A discrete GPU should hold 100%.
- Shadows cost 35-40% of the frame, AO about 1 ms, the ink and upscale passes 2.5 ms. The full story of every bottleneck and optimisation, with numbers, is in `docs/PERFORMANCE.md`.
- **Noise warning:** the laptop throttles (Intel's thermal framework runs). Identical runs differ by up to 2x and a run can be 2x slow for a minute. Trust differences of 30% or more, not 10%. The bench takes the best of three batches for that reason.

### Known issues and limits
- **Vulkan crashes inside `request_device`** on this Intel driver (31.0.101.2135, access violation, even with layers disabled). DX12 works, so Windows defaults to DX12 (`WGPU_BACKEND=vulkan` overrides). Cause not investigated (old driver?).
- **The window size is physical pixels.** A "logical 1280 x 720" window was really 1920 x 1080 at 150% display scaling and ran 2x slower.
- The arena is a bowl: mountains rise to the box top at the edge, so the closed box is mostly hidden. If the camera is buried in rock the picture is flat indigo (dig out with the brush). The camera is held inside the box in height.
- Thin features under about a voxel (0.25 m) cannot exist. Hard `min`/`max` creases came out as sawteeth, so the demo map uses smooth operations. Player craters are still hard-edged and looked fine in the screenshots; a smooth carve (`Op::SmoothCarve`) is a possible later polish.
- The map generator only claims "never over-estimates the distance", enforced by a test (`distance_is_never_overestimated`). Mountain flanks gave a slope-corrected estimate that broke it deep underground; the fix is documented in `world::ground`.
- The empty-space map is per chunk (4 m). Rays that hug the ground spend most of their steps inside the one-chunk shell around the surface.
- No worms, collision against anything but the brush ray, or weapons yet: M1 is a viewer.

### What I could not verify
- **Interactive input was never exercised by a person here.** Mouse capture and look, hold-to-dig, the 20 Hz repeat, window resize, the G/H/O/V/T/R keys: the code paths exist and the window was launched four times (it runs, reaches 60 fps, title updates), but nobody clicked. Rendering and edits are covered by headless screenshots (`--shot --edits`) and the bench's 100-crater loop.
- The art style was judged from screenshots only (`screenshots/M1_overview.png`, `M1_cavern.png`, `M1_craters.png`). Colours and outline weight are a first guess for the owner to react to.
- Other GPUs, drivers, Linux and macOS were not tried. The shaders use no extensions, but timestamp queries are optional (the title then shows no GPU ms and automatic resolution stays at 100%).

### Next decisions (owner)
1. **Review M1.** Run it, dig, fly through the cavern. Is the art direction right (palette, number of toon bands, hard vs soft shadows, outline weight and wobble, strata on the rock)? I will tune from a list.
2. **Speed.** If 60 fps at ~65% is not enough on your target hardware, the next options in order of expected gain: finer empty-space bricks (8 voxels instead of 16, perhaps 25% less), shadows at half resolution, a low-resolution pre-pass for the ray start (helps views down at the ground, not grazing ones).
3. **M0 follow-up:** speed up `Grid::apply` (per-chunk row access instead of per-voxel lookups). Recommendation: when M3 needs it (a tractor beam applies an edit every tick), not now.
4. **Commit** (and whether to keep `screenshots/` in the repo).
5. **Next milestone, M2 (worm in sim):** port `sim` to `Vec3`, capsule collision against `Grid::sample`, jump, dig. It needs no renderer change beyond drawing a capsule worm.

### M0 results (Surface Nets meshing, 2D-to-3D go/no-go), kept for reference
`cargo run -p sdf --release --example bench`, single thread, 128 x 64 x 128 m world. The viewer does not remesh any more, so only the edit rows still matter.
| Measure | Result | Budget |
|---|---|---|
| Build world (16384 chunks) | ~190 ms | |
| Initial mesh, 941k triangles | ~416 ms (0.065 ms/chunk) | |
| r=3 m crater, edit + remesh | 3.2 ms avg, 5.9 ms worst | 2 ms |
| of which edit / remesh | 1.2 ms / 2.0 ms (16 chunks) | |
| Tractor beam (4 m capsule, smooth add) | 1.7 ms/tick | |
| `sample` | ~200 ns | |
| 100 m `raycast` | ~5.6 us | |

The bench example still calls the mesher. Note that `Grid::apply` now marks every changed voxel dirty (the raymarcher needs deep values too), so remesh counts in that example are slightly higher than in the table.

## Verdict: feasible, medium-high effort, main risk is design, not tech
- Technically feasible. Roughly 7.6k lines of Rust here today; the 3D game is probably 8-12k lines new, with the networking/relay layers (~1.5k lines) reusable almost unchanged.
- Biggest risk is **gameplay in 3D** (aiming, camera, orientation in tunnels), not terrain performance. Liero is fun partly because 2D aiming is trivial.
- The video (youtube.com/watch?v=il-TXbn5iMA, Mike Turitzin, "I'm making a game engine based on dynamic signed distance fields (SDFs)") could only be fetched as a title, with no transcript. His public posts (X, 80.lv) say the engine bakes the SDF into voxel bricks and **raymarches** it on WebGPU with clipmap LOD, inspired by Dreams and Claybook. The technique (terrain = SDF, edits = boolean min/max with a moving shape, smooth-min for blending) is well understood, and M0/M1 measured the numbers instead of estimating them.
- Reuse assessment confirmed by a codebase survey: relay `server` (~450 lines), `protocol/relay.rs`, `socket.rs` + `web/net.js`, `Host`, `Throttle`, `Share`/`Loopback`, bincode message flow and test style are dimension-agnostic. `sim` already uses `glam` 0.27, so `Vec3` is a drop-in. 2D-specific and to be replaced: `Tilemap` (361), tile-damage logic in `world.rs`, and nearly all of `game/` rendering (~4.2k lines incl. menus, which can be partly kept in spirit). 61 existing headless tests show the style to keep.
- Known 3D friction in existing code: `Input.aim` is one angle (becomes yaw+pitch), `WeaponDef` units are px with tile-specific fields (`tile_damage`, `Kind::Build`), `Replica` lerp/dead-reckoning/teleport threshold are Vec2, and the **64 KB message cap** means a late-join `Welcome` cannot carry the whole grid in one message: send it as chunked messages or as a compressed edit log + checksums.

## Why Godot/marching cubes was slow, and why Rust should not be
Likely causes: remeshing in script, per-edit allocations, mesh upload, and rebuilding a trimesh collision shape per edit. The fix is architectural, not just "Rust is faster":
1. **Collision never uses a mesh.** The sim samples the SDF directly (distance = penetration, gradient = normal). Worm vs. terrain, projectiles, rope, explosions are all `sample()` / sphere-tracing on a grid. Cheap and deterministic.
2. **No remeshing at all.** The GPU raymarches a texture copy of the grid; an edit re-uploads only the changed chunks (one bounding-box write). (The Surface Nets mesher in `sdf` is kept as a fallback.)
3. **Edits are tiny.** A radius-3 m explosion touches ~14k voxels at 0.25 m: microseconds on CPU.

## Recommended architecture (mirrors this repo)
| Crate | Role | Notes |
|---|---|---|
| `sdf` (new, pure) | chunked i8 distance grid, `carve/add(sphere, capsule, smooth)`, `sample`, `gradient`, `raycast`, parallel `from_fn` map generation, dirty-chunk list, Surface Nets mesher (fallback, unused by the viewer) | no gfx, fully unit-testable; this is the new heart |
| `sim` | `World::step(inputs)`, worms (capsule chain vs SDF), weapons (`WeaponDef` table), tractor beam | Vec3 version of current sim; same "inputs in, state out" rule |
| `host`, `protocol`, `server` | authority, Throttle, Replica interpolation, relay | near-direct reuse; Vec2 -> Vec3, `Input` gains aim yaw/pitch |
| `viewer` (M1, becomes `game`) | wgpu + winit raymarcher, cel shading, ink outlines, brush editing | built; audio, UI, worms and networking glue come with M2-M6 |

Terrain representation (the key decision): **baked chunk grid is authoritative; edits are the network/log format.** The "overlay of SDFs" from the video is evaluated as an ever-growing edit list, whose cost grows with every explosion; baking edits into a grid keeps cost constant, while still sending only `Edit { op, pos, radius }` (~16 bytes) over the wire, the 3D analogue of today's `TileChanged` event (Pattern: Dirty Flag + Event Queue). Distances are stored clamped to a narrow band (about +-4 voxels) in 8 bits; chunks that are entirely air or rock are not stored (Pattern: Spatial Partition).

On the GPU the grid is one 3D texture, kept in step by the dirty-chunk list, plus a small "chunks to the nearest surface" map that lets rays skip empty space.

Tractor beam / "forming": a moving SDF shape applied each tick as add (smooth-min) or subtract. Shaping at 60 Hz touches a few chunks per tick, which the dirty-chunk remesh budget covers. Flying debris chunks (carved rock becoming physical objects) is a separate hard feature; skip, use particles.

## Rendering / art style
- Terrain: **raymarched** per pixel from the 3D texture (see Status for how), toon ramp of 3 flat bands, procedural strata and grass, no external assets. Triplanar paper texture is not done.
- Ink outlines (Okami feel): post-process on a depth + normal G-buffer, built. Line width and wobble are modulated by noise. Worms and projectiles will draw into the same G-buffer, so they get outlines for free; an inverted-hull outline for characters stays an option.
- Worms: chain of spheres/capsules generated in code (matches "no external assets" and Liero's look). They are rasterised and depth-tested against the raymarched depth, or marched as analytic capsules in the same shader (decide at M2).
- Alternative that was NOT taken: Surface Nets meshes. Cheaper per pixel but faceted at 0.25 m voxels, and the toon bands and outlines make facets show. It is still in `sdf::mesh` as a fallback.
- Stack: `wgpu` 30 + `winit` 0.30 (native now, WebGPU later, untested). Bevy rejected: heavy, ECS conflicts with the project's "plain structs, no ECS" rule. `egui` for menus/debug is still planned and not added (the title bar carries the stats for now).

## Performance budget (measured at M1, see Status for the table)
- Edit of an r=3 m crater: CPU 1.0-1.7 ms (budget 2 ms) + 2-4 ms main-thread upload. No remesh.
- Sim step (4 worms, ~50 projectiles, a few edits): < 1 ms. Not yet built.
- World memory: 64 MB texture + ~14 MB on the CPU (3.4k of 16k chunks hold data), so ~80 MB. Budget was < 100 MB.
- Render: **60 fps at 1280 x 720 with ~65% internal scale on an Intel UHD laptop iGPU**, everything on. Not met at 100% on that GPU (18-23 ms, see Results). Budget "60+ fps on integrated at 1080p" needs the speed options in Next decisions, or accepting the automatic scale.

## Netcode implications
- Almost everything reusable: host-authoritative, `Host`/`Throttle`, `Replica` 100 ms interpolation, relay server, WebSocket wrapper.
- Snapshot grows from 2D to 3D pos + orientation (~600 B at 4 players; still ~18 KB/s at 30 Hz).
- Terrain sync: late-join `Welcome` sends the compressed grid (or the edit log if short); afterwards only `Edit` events. Clients apply edits with the same integer/IEEE-exact code (min/max/sqrt, avoid fast-math); add a per-chunk checksum in snapshots and resend a chunk on mismatch.
- Still no client prediction in v1; 3D mouse-aim at ~175 ms perceived latency will feel worse than 2D, so plan an early playtest to decide whether to add prediction for the local worm.

## Risks (ranked)
1. **3D aiming/camera/worm orientation fun** - mitigate: playable M2 prototype early, aim assist, tunnel-scale arenas, consider a 2.5D fallback mode.
2. SDF validity after repeated min/max edits (distances become approximate; raymarching overshoots if a stored value over-estimates) - mitigate: narrow band + occasional local redistancing; the generator has a Lipschitz test, edits do not yet. Hard creases are sawtoothed at 0.25 m, so smooth operations are preferred.
3. Ink-outline art tuning - isolated, can iterate indefinitely.
4. Terrain desync online - checksums + chunk resync.
5. wgpu boilerplate/learning curve and WebGPU browser support later - acceptable; web is explicitly deferred. New at M1: driver bugs (Vulkan crash on the dev laptop) and a throttling iGPU that makes benchmarks noisy.

## Milestones (each ends runnable, owner reviews before next)
- **M0 Terrain spike (go/no-go): DONE.** `sdf` crate + tests + timing harness. The edit cost (1.0-1.7 ms) is the open item.
- **M1 Viewer: BUILT, awaiting review.** wgpu window, fly camera, click-to-carve/build, raymarched toon shading, ink outlines, automatic resolution. Gate: art style approved.
- **M2 Worm in sim:** capsule-vs-SDF movement, jump, dig, rope, `Input` with aim; headless tests for determinism.
- **M3 Weapons:** port `WeaponDef` table, projectile raycast, explosions = carve, build = add, tractor beam.
- **M4 Host + loopback + Replica (Vec3):** offline play via the same pipeline as the 2D game.
- **M5 Online:** relay (reused), late join, edit events, checksums.
- **M6 Polish:** synthesized audio, bots, procedural maps (SDF primitives + noise caves), HUD.

## Fork plan: copy this repo, keep what fits, delete the rest
Step 0 (first thing after approval): `git clone` this repo to a sibling folder (e.g. `C:\Users\TE\Documents\liero_3d`), keep history (so deleted 2D code stays readable via `git log`/`git show`), remove the `origin` remote so nothing is pushed to the 2D repo by accident. Update `CLAUDE.md`/`PLAN.md` for the 3D game (same style rules, new terrain/rendering sections).

| Keep untouched | Keep, port to Vec3 later | Delete in the fork |
|---|---|---|
| `server/` (relay, Dockerfile, fly/render configs), `protocol/src/relay.rs`, `host/src/throttle.rs`, `sim/src/rng.rs`, `docs/DEPLOY.md`, test style | `sim/{input,world,weapons,bots}.rs`, `protocol/{lib,replica}.rs`, `host/src/lib.rs`, `game/src/{share,local,join,net}.rs`, `docs/NETWORKING.md` | `sim/tilemap.rs`, `game/` rendering and UI built on macroquad (`terrain, shaders, lights, draw, effects, view, present, touch, ink, hud, menu, setup, armory, controls`), `web/gl.js` (vendored macroquad glue; irrelevant until web returns) |

Keeping the build green: the "port later" crates stay in the tree but are removed from workspace `members` until their milestone (M2-M5), so `cargo test` keeps passing while `sdf` is built first. Network glue (`share/local/join/net`) moves into the new `game` crate when it is rewritten. Files I have not read in detail (`ink.rs`, `menu.rs`, `setup.rs`) get a quick look before deletion in case something is worth keeping as reference.

## Existing code that a Rust library would do better
Recommendations, ordered by value. Crate versions and APIs are from memory/search and must be checked on crates.io when adopting.
| Need | Hand-rolled today / planned | Better: use | Verdict |
|---|---|---|---|
| Terrain meshing | (would be custom) | `fast-surface-nets` | Adopted at M0, **not used by the viewer** since M1 raymarches. Kept as a fallback |
| Parallel remeshing | n/a | `rayon` (or `std::thread` + channel for budgeted per-frame jobs) | Adopt |
| Cave/map noise | n/a | `noise` or `fastnoise-lite` | Adopt (procedural maps) |
| Window + GPU | macroquad | `winit` + `wgpu` | **Adopted at M1** (wgpu 30, winit 0.30; multipass, timestamp queries and 3D textures all used; web via WebGPU later) |
| Menus/debug UI | `menu/setup/armory/hud.rs` (~1k lines, macroquad-bound) | `egui` + `egui-wgpu` for menus, lobby, debug panels; keep the HUD hand-drawn in the ink style | Adopt for menus/debug only |
| Audio | (synth in code) | `cpal` + your own synth, or `fundsp`; `kira` if you want mixing/spatial for free | Optional |
| Gamepad | none | `gilrs` | Adopt later |
| Collision/physics | `Tilemap` circle/line tests | Do **not** adopt `rapier`/`parry` for terrain: direct SDF sampling is simpler, faster and deterministic. Parry only if you later want worm-vs-worm rigid shapes | Skip |
| WebSocket client | `socket.rs` (194) + `web/net.js` (67) + tungstenite without TLS | `ewebsock` (one API for native + wasm, native TLS so it can reach `wss://` relays, fixing the documented "native speaks ws:// only" limit) | **Consider**: replaces ~260 lines, but is a new dependency; do at M5 |
| Unreliable transport / no head-of-line blocking | WebSocket relay (TCP) | `matchbox_socket` (WebRTC data channels, peer-to-peer with a small signalling server, native + web) or `renet`/`quinn` for native UDP | **Evaluate at M5**: matchbox would replace relay rooms + socket glue and give unreliable channels, but it replaces tested code, so keep the relay until the game is fun |
| Message encoding | `bincode` 1.3 | `postcard` (compact varints) or `bincode` 2 | Low value; bincode 1.3 is in maintenance mode, swap if you want smaller packets |
| Chunk compression for late join | none | `lz4_flex` (fast, pure Rust, wasm-safe) | Adopt at M5 |
| Math | `glam` 0.27 | keep (add `Vec3`, `Quat`) | Keep |
| Server | axum + tokio | keep | Keep |
| Bots | hand-written steering | keep; later navigation in SDF space is custom anyway | Keep |

## Next step
Owner review of M1 (Status, "Next decisions"). Then M2 (worm in sim) or, if the look needs work, a tuning round first. Stop and report before M2.
