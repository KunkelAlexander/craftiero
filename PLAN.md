# 3D SDF Liero: plan (the 2D plan is in docs/PLAN_2D.md)

## Context
Goal: a 3D, Liero-style worm shooter with fully destructible/formable terrain based on signed distance fields (SDFs), cel-shaded with Okami-like ink outlines, smooth fast gameplay first, possible online multiplayer, Rust-native, structured like this repo. Prior attempt (voxellini, marching cubes in Godot) was too slow.
Decisions from the user: **native first, web later**; **full 3D volume, small arena** (caves/tunnels, ~128x64x128 m at ~0.25 m voxels); **raymarch the SDF, do not mesh it** (decided at M1, see Status).

## Status (read this first)
- **Done:** fork + prune of the 2D repo (history kept), `CLAUDE.md` rewritten for 3D, **M0** the `sdf` crate, **M1** the `viewer` tech demo (committed), **M2** a worm in the `sim` crate with a third-person mode in the viewer (committed), **M3** weapons: eleven weapons as table rows, projectiles, explosions that carve, damage and respawning, effects in the viewer. **M3 is built and tested, and waits for the owner's review** (feel and balance above all: see "M3" below).
- **Workspace:** `sdf`, `sim` and `viewer` are `members`. `sim/old2d/` holds the 2D bots for M6 (its weapons and world were ported at M3 and deleted). `protocol`, `host`, `server`, `game/src/{share,local,join,net,socket}.rs` are 2D code kept for porting and do not compile yet. Re-add each at its milestone.
- **Not committed:** the M3 work (`sim/src/{weapons,combat}.rs`, `viewer/src/effects.rs`, input and bench changes, the docs). M0 to M2 are committed.
- **Tests:** `cargo test -p sdf -p sim -p viewer` (10 + 48 + 11 headless tests, all pass).

### M2: the worm (built)
What exists: `sim::World` (grid + worms, `step(&[Input])` at a fixed 60 Hz, events drained by consumers), `sim::Worm` (capsule of three spheres, walk, jump, dig, rope), a 3D `Input` (move, absolute yaw and pitch, jump, dig, rope, reel; sanitised against NaN and huge values), a third-person camera that is pulled in by rock, worms drawn as capsules in the raymarcher, fixed-timestep loop with drawing between ticks. The 2D `sim` files moved to `sim/old2d/`; only `input.rs` and `rng.rs` survive in `sim/src`.

Design decisions, each with its reason in the code comments:
- **Collision:** each body sphere reads one distance, pushes out along the gradient; walking happens in the ground's plane (a horizontal walk into a slope lost a quarter of its speed per tick, measured); the ground normal is taken at the feet, not at the foot sphere's centre (at a wall-floor crease the sphere's gradient points diagonally and ground detection flickered); a worm is pulled onto ground within 10 cm instead of hovering; ledges up to 0.5 m are stepped up.
- **Dig:** radius 1.5 m centred on the surface point, 5 per second, reach 4 m. The size has a floor that is derived, not tuned: bites a radius apart leave a roof cusp of 1.73 r, which must exceed the worm's 1.8 m (a 1.1 m radius pinched the worm at every joint, found by a test).
- **Rope:** press shoots (raycast along the aim, 30 m), press again lets go; a taut rope is a hard distance limit that cancels outward speed, so swinging is a pendulum and speed is kept on release (air control never brakes); the rope lets go if its rock is dug away. Dynamics pass: reeling in (E) also accelerates the worm towards the anchor (45 m/s^2, up to 20 m/s) on top of shortening the rope at 12 m/s; steering on the rope pushes harder (30 m/s^2) with a 24 m/s cap, so it pumps the swing; Space in mid-air on the rope lets go and kicks up 7 m/s. A test shows a worm that cannot jump out of a 10 m pit (rose under 4 m in 5 s) rope-launching itself out.
- **Movement feel (dynamics pass):** walk 8 m/s with 70 m/s^2 ground acceleration; jump 11 m/s (2.4 m held, a tap hops about half a metre); falling gravity x1.5 (not on a rope); coyote time and jump buffer of 0.1 s each; holding Space bunny-hops on every landing; speed above walking speed is only gently dragged while steering (10 m/s^2) but fully braked with no keys; air control 20 m/s^2 up to walking speed, never braking. The view widens up to +14 degrees of field of view above 8 m/s.
- **Uneven ground:** walls push sideways only (a tilted push lifted a worm running into a wall, and at the wall-floor crease it climbed the wall); ground slopes up to 41 degrees count as ground; a walking worm sticks to ground up to 0.3 m below it instead of launching off the crest of every bump (it was airborne 102 of 120 ticks on 0.15 m bumps before); ledges up to 0.5 m are stepped up. Tests: bumpy ground keeps the pace, and a worm walks back out of the tunnel it dug.
- **Determinism:** same inputs give the same worm bit for bit and the same terrain checksum on one machine (test). `sin`/`cos` are not guaranteed identical across CPUs, which only matters if a client ever predicts; noted in `world.rs`.
- **Rendering:** capsules are tested with a closed-form ray test in the march shader; worm bodies shadow the terrain; per-screen-tile bitmasks keep the cost at +0.6 ms for four worms (a plain loop cost 6-10 ms, `docs/PERFORMANCE.md` section 8).

How to look at it without a window: `-- --shot out.png --worm 64,38,200 --cam 64,19.5,32,0,-0.12` (add `--rope` for a swinging worm; `--worm 64,22,180 --rope --cam 76,28,26,270,-0.05` shows one hanging on a mountain).

### M3: weapons (built)
What exists: `sim::weapons` (the `WeaponDef` table: 11 weapons plus the cluster's bomblet, ported from the 2D game and converted to metres), `sim::combat` (firing, projectiles, explosions, damage, respawn, as methods of `World`), `Input` gained `fire` and `weapon` (an id, sent every tick like the aim), worms have health, a magazine per weapon, kills and deaths, `World::new(grid, spawns, seed)` (the seed starts the world's own random numbers; the spawn points are also where a dead worm comes back). In the viewer: `effects.rs` (projectiles as capsules, a fireball per explosion, a line per beam, all started by the sim's events), the number row and the mouse wheel pick the weapon, Left Shift digs, the window title is the HUD until M6.

The arsenal (an id is also its slot): Blaster (10 damage, 20 rounds), Rocket (3 m blast, 2.5 m crater), Railgun (a beam that pierces worms and stops at rock), Bouncer (ricochets, mirrored in the distance field's gradient), Bomb (dropped at your feet, falls and comes to rest, goes off after 1.2 s or when an enemy touches it), Cluster (a lobbed shell that flies through worms and bursts into 7 bomblets), Swarm (6 missiles that curl in on the nearest enemy), Teleport (an orb that moves you to where it lands, or swaps you with the worm it hits), Nova (8 m blast, 7 m crater, 8 s reload), Block ("build = add": a 1.4 m lump of rock where you aim, never inside a worm or on a spawn point), Tractor (the forming tool: a stream of 0.8 m lumps, 16 a second, to draw a bridge or a wall by sweeping the aim).

Design decisions, each with its reason in the code comments:
- **Explosions are terrain edits.** A blast hurts worms first and carves the crater after, so the rock a blast removes still shields you from that very blast (test). Damage falls off linearly and needs line of sight: a ray from the blast to the nearest point of the body (test: the same bomb hurts in the open and not behind a 2 m slab).
- **Knockback pushes away and a little up** (35% lift). Standing worms cancel the speed into the ground, and ground friction would eat a sideways push within a few ticks; with the lift a rocket at your feet launches you (test: rises more than 6 m, and costs between a fifth and a third of your health, half of what the same blast does to anyone else).
- **Projectiles are points with a 12 cm radius** stepped in 25 cm pieces through the distance field (a ridge of rock is a few voxels thick, a bolt moves a metre a tick). A ricochet mirrors the velocity in the gradient, so it works on any terrain shape. Bombs are the same code with gravity and a low bounce, and creep to rest.
- **Found by a test:** a bomb at rest is always touching the ground, and the ricochet branch ended with `continue`, which skipped the worm test, so no mine could ever trigger. Fixed by falling through.
- **The tractor extrudes rock towards you.** Each lump is placed on the rock the last one made, so at one lump per tick the pile reached the worm in under a second. It now fires every fourth tick (about 8 m/s) and stops short of your face (the "never touch a worm" rule); a test checks it.
- **The 2D rules were kept** where they still made sense: self damage is half but knockback is full, spawn protection ends when you shoot, a beam has slack so a joiner's lag does not make the host's shots land more often, a cluster shell flies through worms.
- **Determinism:** the same fight (clusters, swarms, rockets, bouncers, 400 ticks) plays out identically twice (test). The homing turn uses a quaternion (sin/cos), the same caveat as the walk direction.

Numbers (release build, this laptop):
| Measure | Result |
|---|---|
| `Grid::apply` alone: r = 1.5 / 3 / 8 m carve | 0.12 / 0.46 / 4.2 ms (the Nova is the only one that is more than a fraction of a frame) |
| tractor lump (r = 0.8 m, smooth) | 0.11 ms |
| in the viewer: r = 3 m crater, CPU edit + upload | 0.7 + 1.5 ms |
| in the viewer: r = 8 m crater, CPU edit + upload | 8.8 + 2.2 ms (one frame hitch per Nova, once per 8 s reload) |
| rewriting `Grid::apply` to own each chunk once | 5-8% faster, **not kept** (the time is the distance maths per voxel, not the lookups) |

This settles the M0 follow-up: the faster `Grid::apply` that M3 was expected to need is not needed. If the Nova hitch matters, the next idea is to skip chunks that lie entirely inside a carve sphere (set them to air without evaluating a voxel); about a third of a Nova's chunks are like that.

How to look at it without a window: `-- --shot out.png --worm 64,38,0 --cam 56,24,24,32,-14 --fire 1,50` fires weapon 1 (the rocket) and takes the picture 50 ticks later (`--fire id[,ticks]`).

### Run it
```
cargo run -p viewer --release
```
Click the window to capture the mouse, Esc releases it (again to quit).

| Input | Does |
|---|---|
| W A S D, mouse | walk, look and aim |
| Space | jump |
| left mouse (hold) | fire the current weapon |
| 1 2 3 4 5 6 7 8 9 0 -, wheel | pick a weapon (Blaster, Rocket, Railgun, Bouncer, Bomb, Cluster, Swarm, Teleport, Nova, Block, Tractor) |
| Left Shift (hold) | dig, five holes a second |
| right mouse | shoot the rope / let go |
| E / Q | reel the rope in / out |
| F | fly camera with the M1 brush (below) |
| *fly:* W A S D, Space / Ctrl, Shift | fly, up / down, fast |
| *fly:* left mouse (hold) | carve a sphere at the crosshair, repeats at 20 Hz |
| *fly:* right mouse (hold) | build: a sphere pushed out along the wall, blended in like clay |
| *fly:* wheel | brush radius (0.5 to 8 m) |
| F1 / F2 / F3 | fixed render resolution 100% / 75% / 50% |
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
- **Noise warning:** timings on this laptop drift (background CPU load and the power mode were found to matter; thermal throttling was suspected and never measured). Identical runs differ by up to 2x and a run can be 2x slow for a minute. Trust differences of 30% or more, not 10%. The bench takes the best of three batches for that reason.

### Known issues and limits
- **Vulkan crashes inside `request_device`** on this Intel driver (31.0.101.2135, access violation, even with layers disabled). DX12 works, so Windows defaults to DX12 (`WGPU_BACKEND=vulkan` overrides). Cause not investigated (old driver?).
- **The window size is physical pixels.** A "logical 1280 x 720" window was really 1920 x 1080 at 150% display scaling and ran 2x slower.
- The arena is a bowl: mountains rise to the box top at the edge, so the closed box is mostly hidden. If the camera is buried in rock the picture is flat indigo (dig out with the brush). The camera is held inside the box in height.
- Thin features under about a voxel (0.25 m) cannot exist. Hard `min`/`max` creases came out as sawteeth, so the demo map uses smooth operations. Player craters are still hard-edged and looked fine in the screenshots; a smooth carve (`Op::SmoothCarve`) is a possible later polish.
- The map generator only claims "never over-estimates the distance", enforced by a test (`distance_is_never_overestimated`). Mountain flanks gave a slope-corrected estimate that broke it deep underground; the fix is documented in `world::ground`.
- The empty-space map is per chunk (4 m). Rays that hug the ground spend most of their steps inside the one-chunk shell around the surface.
- The M1 brush edits the grid directly and bypasses the sim (fly mode only, a developer tool); worm digging goes through `World::step`.
- There are no match rules yet (score limit, winner, loadout choice: every worm carries all 11 weapons) and no sound or HUD beyond the title bar. The second worm is an idle dummy that dies and respawns; it never shoots back.
- Weapon values are first guesses and unbalanced: 3D is harder to hit than 2D and the 2D damage numbers were kept, so the Blaster is probably weak. They are table rows in `sim/src/weapons.rs`.
- A dead worm is simply not drawn (plus a flash); the camera stays where the body fell. Capsules cap at 32 on screen (`MAX_CAPSULES`): worms first, then projectiles, then effects, so a busy fight drops the newest fireballs first.
- The arena has an invisible ceiling at 64 m (outside the grid counts as rock), and the camera is held under it.
- A worm has no ragdoll or tilt: the capsule always stands upright.
- The M1 edit cost finding, large brushes (r = 3 to 8 m) costing 7-37 ms of CPU per edit in a logged session, is explained by the r = 8 m number above (about 9 ms in the viewer, 4 ms in the bare sim) plus background load. A Nova costs one such hitch.

### What I could not verify
- **M2 feel.** I could not play it. The numbers (walk 8 m/s, jump 2.4 m, gravity 25, dig 5 per second, camera 5 m behind) are first guesses written down as constants at the top of `sim/src/world.rs`; the tests prove they are consistent (a worm stands, climbs, jumps 2 m, digs a tunnel it can walk through, swings), not that they are fun. The window was smoke-run for 8 s (no panic or GPU validation error), and screenshots show the worm, eyes, gun, shadow and a rope swing. Nobody has steered it with a mouse. Whether third-person aiming feels right (the crosshair marks the camera's ray; the dig ring marks where the worm's eye ray lands, slightly different) is the main open question.
- **M3 feel and balance.** I could not play it. The sim tests prove the mechanics (damage, falloff, cover, ricochet, homing, teleport, building, respawn, determinism), and screenshots (`--fire`) show a rocket in flight, a fireball over a fresh crater and a Nova. Not verified: how the weapons feel and balance, how aiming a rocket at a moving worm in 3D plays, the mouse-wheel and number-key weapon switching (the code compiles and the window starts, nobody pressed them), how the tractor feels in the hand, and the frame cost of a big fireball covering much of the screen with many capsules (the worm benchmark ran with 17 capsules, not 32 and not with a screen-filling one).
- **Interactive input was never exercised by a person here.** Mouse capture and look, hold-to-dig, the 20 Hz repeat, window resize, the G/H/O/V/T/R keys: the code paths exist and the window was launched four times (it runs, reaches 60 fps, title updates), but nobody clicked. Rendering and edits are covered by headless screenshots (`--shot --edits`) and the bench's 100-crater loop.
- The art style was judged from screenshots only (`screenshots/M1_overview.png`, `M1_cavern.png`, `M1_craters.png`). Colours and outline weight are a first guess for the owner to react to.
- Other GPUs, drivers, Linux and macOS were not tried. The shaders use no extensions, but timestamp queries are optional (the title then shows no GPU ms and automatic resolution stays at 100%).

### Next decisions (owner)
1. **Review M1.** Run it, dig, fly through the cavern. Is the art direction right (palette, number of toon bands, hard vs soft shadows, outline weight and wobble, strata on the rock)? I will tune from a list.
2. **Speed.** If 60 fps at ~65% is not enough on your target hardware, the next options in order of expected gain: finer empty-space bricks (8 voxels instead of 16, perhaps 25% less), shadows at half resolution, a low-resolution pre-pass for the ray start (helps views down at the ground, not grazing ones).
3. **M0 follow-up (settled at M3):** a faster `Grid::apply` was tried and is not needed (see "M3" numbers).
4. **Commit** (and whether to keep `screenshots/` in the repo).
5. **Review M2.** Play it: walk, jump, dig a tunnel, swing from the ceiling of the cavern. Tell me what feels wrong (speeds, camera distance and height, dig size and rate, rope length and reel speed); they are single constants. Also: is the worm's look (capsule, eyes, gun) the direction you want?
6. **Review M3.** Play it: pick weapons with 1 to 0 and `-`, shoot the dummy worm and watch it respawn, rocket-jump (aim a rocket at your feet), hide behind rock from a Nova, build a bridge with the tractor, dig with Left Shift. Tell me what feels wrong; every number is a row in `sim/src/weapons.rs`. Answered: a match gets a score limit and a winner, and all 11 weapons stay (see the decisions below). Still open: should a dead worm's camera do something (spectate, slow zoom)?
7. **Next milestone, M4 (host, loopback, replica):** offline play through the same pipeline as online.

### Owner decisions after reviewing M3 (to implement another time)
- **A match has a score limit and a winner.** Port the 2D rule: first to N kills wins (the 2D default was 15, `SCORE_LIMIT`), `World::winner`, an `Event::MatchWon`, and the world freezes once someone has won (`step` returns early). Killing yourself costs a point, as in 2D (already implemented: `kills` goes down). The limit is a rule of the match, set by whoever creates the world. Not started: `combat.rs` has the kills and deaths but no winner yet.
- **For now every worm carries all 11 weapons** (the current behaviour: one slot per `ARSENAL` entry, `Input::weapon` is an id). The loadout of five picked on a menu is dropped for now; it can come back with the menus (M6) if wanted.
- **The ported 2D files are deleted:** `sim/old2d/weapons.rs` and `world.rs` (done). `sim/old2d/bots.rs` stays for M6.

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
5. wgpu boilerplate/learning curve and WebGPU browser support later - acceptable; web is explicitly deferred. New at M1: driver bugs (Vulkan crash on the dev laptop) and an iGPU whose timings drift (background load, power mode) so benchmarks are noisy.

## Milestones (each ends runnable, owner reviews before next)
- **M0 Terrain spike (go/no-go): DONE.** `sdf` crate + tests + timing harness. The edit cost (1.0-1.7 ms) is the open item.
- **M1 Viewer: DONE (committed).** wgpu window, fly camera, click-to-carve/build, raymarched toon shading, ink outlines, automatic resolution. Gate: art style approved.
- **M2 Worm in sim: DONE (committed).** Capsule-vs-SDF movement, jump, dig, rope, `Input` with aim, headless tests (including determinism), third-person viewer mode.
- **M3 Weapons: BUILT, awaiting review.** `WeaponDef` table (11 weapons), projectiles through the distance field, explosions = carve, build = add, tractor = a rapid stream of small adds, damage, cover, respawn, effects in the viewer.
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
Owner review of M3 (Status, "Next decisions"): play it, then either a tuning round (weapon numbers, camera, aiming) or M4. Stop and report before M4.
