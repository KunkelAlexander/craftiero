# 3D SDF Liero (working title)

A real-time 3D worm shooter in the spirit of *Liero*, on **fully destructible,
formable terrain stored as a signed distance field (SDF)**. Rust, wgpu, no
external assets. Cel shaded, with Okami-like ink outlines. Smooth, fast
gameplay beats graphics.

It is also a learning project: the code is commented to explain *why*, names the
patterns from *Game Programming Patterns* where they are used, and will explain
the networking concepts where they appear.

**Where it is:** milestone M1 of seven. You can fly through a cave-and-hills
world and dig, build and blast it with the mouse at 60 fps on an integrated
laptop GPU. There are no worms, weapons or multiplayer yet (those are M2-M6, see
[PLAN.md](PLAN.md)).

| | |
|---|---|
| ![Overview: a bowl-shaped arena with floating rocks, a few craters and a built-up blob.](screenshots/M1_overview.png) | ![Inside the cavern: pillars, roof holes lit by the sun.](screenshots/M1_cavern.png) |
| *The arena. Craters and the raised blob were made with the brush.* | *Inside the cavern, lit by a warm headlamp where the sun does not reach.* |

![Craters carved into a steep mountain flank, each one a clean spherical bite through the rock strata.](screenshots/M1_craters.png)

*Craters in a mountain flank. The orange disc is the brush.*

## Run it

You need Rust (stable) and a GPU with a DX12, Vulkan or Metal driver.

```
cargo run -p viewer --release
```

Click the window to capture the mouse; Esc releases it, Esc again quits.

| Input | Does |
|---|---|
| W A S D, Space / Ctrl, Shift | fly, up / down, fast |
| left mouse (hold) | carve a sphere at the crosshair |
| right mouse (hold) | build: new material pushed out along the wall, blended in like clay |
| mouse wheel | brush radius |
| 1 / 2 / 3 | fixed render resolution 100% / 75% / 50% |
| T | automatic resolution (default): scales the resolution to hold ~60 fps |
| G / H / O / V | shadows / ambient occlusion / ink outlines / vsync |
| R | regenerate the map (about half a second) |

The window title shows fps, GPU milliseconds per pass, the cost of the last edit
and upload, the brush size and the render scale.

Headless modes (no window needed):

```
cargo run -p viewer --release -- --shot out.png --edits --cam 62,36,54,180,-12
cargo run -p viewer --release -- --bench --size 1920x1080
cargo test -p sdf -p viewer
```

On Windows the viewer uses DX12 because Vulkan crashed on the Intel driver it was
developed on. Set `WGPU_BACKEND=vulkan` (or `metal`, `gl`) to override.

## How it works

The terrain is a grid of distances to the nearest surface: positive in air,
negative in rock, zero on the surface, one signed byte per 0.25 m voxel, in
16-voxel chunks (a 128 x 64 x 128 m world is 64 MB). Digging is
`max(terrain, -shape)`, building is `min(terrain, shape)` or a smooth minimum,
so an edit is a tiny value (a shape and an operation), which is also what will go
over the network.

* **[`sdf/`](sdf/)** is a pure crate (no window, GPU, clock or I/O): the grid,
  edits, `sample` / `gradient` / `raycast` for collision, parallel map
  generation, dirty-chunk tracking, checksums. Collision will sample the grid
  directly; there is no collision mesh.
* **[`viewer/`](viewer/)** is the M1 tech demo. It copies the grid into a 3D
  texture and **raymarches it on the GPU** (sphere tracing), shades it with a
  three-band toon ramp, and draws ink outlines from a depth/normal buffer.
  After an edit only the changed chunks are re-uploaded. Read it in this order:
  `main.rs` (window, input, brush), `renderer.rs` (textures, passes, uploads, GPU
  timers), `shaders.rs` (the WGSL), `world.rs` (the demo map), `camera.rs`.

Why raymarch instead of meshing the terrain? The first plan used Surface Nets
meshes. At 0.25 m voxels a mesh looks faceted, and toon bands plus outlines make
facets show. The author of the video that inspired this project raymarches baked
SDF bricks, so M1 does too. Details and what could not be confirmed are in
[PLAN.md](PLAN.md).

## Performance

On an Intel UHD laptop GPU at 1280 x 720 the frame went from roughly 37-67 ms to
18-23 ms at full resolution (the early numbers are less precise, see the log), edit uploads from 23 ms to 2-4 ms, and map generation from
19.5 s to 0.6 s. Everything on, it holds 60 fps at about 65% internal
resolution (automatic).

The full log is in **[docs/PERFORMANCE.md](docs/PERFORMANCE.md)**: every
bottleneck found (too many texture fetches per pixel, shadow rays, per-chunk
uploads, an outline pass that ignored the render scale, a 19.5 s map generator,
high-DPI scaling), each optimisation with before/after numbers, what did not
help, how noisy the measurements are on a throttling laptop, and what to try
next.

## Roadmap

Milestones (full detail and decisions in [PLAN.md](PLAN.md)):

| | |
|---|---|
| M0 | `sdf` crate and terrain spike: **done** |
| M1 | Raymarched viewer with brush editing: **built, awaiting review** |
| M2 | A worm in the sim: capsule against the SDF, jump, dig, aim |
| M3 | Weapons as data rows: explosions carve, build adds, tractor beam |
| M4 | Host + loopback + replica: offline play through the same pipeline as online |
| M5 | Online: relay server, late join, edit events, checksums |
| M6 | Polish: synthesized audio, bots, procedural maps, HUD |

## Repository

| Path | What |
|---|---|
| `sdf/`, `viewer/` | the 3D code (in the workspace) |
| `sim/`, `protocol/`, `host/`, `server/`, `game/src/{share,local,join,net,socket}.rs` | the 2D game's simulation, networking and relay, kept to be ported at M2-M5; they do not compile yet |
| [`PLAN.md`](PLAN.md), [`CLAUDE.md`](CLAUDE.md) | the plan and decisions; the working rules for the code |
| [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) | performance log |
| [`docs/NETWORKING.md`](docs/NETWORKING.md), [`docs/DEPLOY.md`](docs/DEPLOY.md) | written for the 2D game's relay; they apply again from M5 |
| [`docs/PLAN_2D.md`](docs/PLAN_2D.md) | the 2D game's plan |
| `screenshots/` | M1 screenshots used above |

This project began as a fork of **Bomblero**, a 2D neon-cave arena shooter
(macroquad, browser and native, online through a relay). Its history is kept: the
2D game is in the git history from before the fork's changes.

## License

MIT, see [LICENSE](LICENSE).
