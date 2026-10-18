# Performance log (M1 raymarched viewer)

What was slow, why, what I did about it, and how much it helped. Everything here
was measured on one machine, and that matters, so read "How to trust these
numbers" first.

## How to trust these numbers

- **Machine:** a laptop with an Intel UHD integrated GPU (shared memory, Intel
  driver 31.0.101.2135), Windows 11, DX12 backend, `cargo --release`.
- **Timings drift.** Intel's thermal framework (`dptf_helper`) was running, and
  identical runs differed by up to 2x. Twice the whole machine was ~2x slow for a
  minute (even the CPU-only map generation took 1.3 s instead of 0.6 s). I first
  put this down to thermal throttling, which was never measured; section 9 found
  a real cause (background CPU load sharing the power budget). Treat differences
  under about 30% as noise.
- **Two measuring methods were used.** Early numbers are the wall-clock time of
  "submit, wait for the GPU, return", averaged over one batch. Later numbers use
  the GPU's own timestamp queries (per pass) and the best of three batches. The
  second method is more trustworthy, so a "before (wall clock) vs after
  (timestamps)" comparison slightly flatters the result. The direction and
  rough size of every win below were also confirmed by repeated runs.
- **Views.** *open hills* is the spawn camera looking down at the bowl, *cavern*
  is inside the cave, *ground level* is 5 m above the ground looking along it
  (grazing rays, the worst case). The scene is the demo map, 128 x 64 x 128 m.
- Reproduce with `cargo run -p viewer --release -- --bench [--size 1920x1080]
  [--scale 0.5]`. `BENCH_FRAMES=n` sets the batch size (default 40).

## The cost model: where a frame's time goes

One frame is three full-screen passes.

1. **March:** for every pixel, sphere-trace the signed distance field, then
   shade (normal, hard shadow ray, ambient occlusion, toon ramp). About 85% of
   the GPU time.
2. **Ink:** edge detection on the depth/normal G-buffer.
3. **Present:** upscale, grain, vignette, crosshair.

Final split at 1280 x 720, 100% scale, open hills: march 16.1 ms (of which the
shadow ray is about 7 ms and ambient occlusion about 1 ms), ink + present 2.5 ms.

## Bottlenecks found, in the order I hit them

### 1. Too many texture fetches per pixel (the marcher itself)

*Symptom.* First working version, 1080p: **69 ms (open hills), 85 ms (cavern),
113 ms (ground level)** per frame; 720p: 37 / 55 / 67 ms. A debug heat map of
steps per pixel showed about 25-40 fine steps (each one a trilinear fetch from a
64 MB 3D texture) and 8+ coarse jumps per pixel.

*Why.* Sphere tracing advances by the distance the field promises is free, and
the stored distance is clamped to 1 m (+-4 voxels, to fit a byte). A ray
crossing open air near the ground takes about one step per metre. A ray that
approaches the ground at a shallow angle takes many more: each step only
shrinks its height above the ground by `step * sin(angle)`, so the ray
converges geometrically (roughly 60 steps for a 5 degree grazing ray from 1 m
up).

**Optimisation 1a: empty-space skipping.** A 32 x 16 x 32 byte texture stores,
for every 16-voxel chunk, the chessboard distance (in chunks) to the nearest
chunk that holds a surface. A ray standing in a chunk with value `k >= 2` knows
nothing solid is nearer than `k - 1` chunks, so it jumps that far with one
cheap fetch from an 8 KB texture instead of dozens from the big one. Built by a
two-sweep distance transform; unit-tested against brute force. This is part of
the first version above, not a separate before/after: without it a ray across a
128 m arena costs 128+ fetches, so the demo would not run at all.

**Optimisation 1b: hit tolerance of half a pixel, and a step floor equal to
it.** The first version stopped a ray within 4 mm + 0.06% of its distance of the
surface, far finer than a pixel can show: at 60 m a pixel covers 12 cm. The ray
now stops once it is closer to the surface than half a pixel's footprint and
never steps less than that, which removes the long crawl of grazing rays.

| 720p, wall clock | open hills | cavern | ground level |
|---|---|---|---|
| before | 36.7 ms | 54.6 ms | 66.7 ms |
| after 1b | 24.6 ms | 23.6 ms | 39.3 ms |
| march pass only, before | 25.2 | 43.0 | 50.0 |
| march pass only, after 1b | 12.2 | 16.1 | 27.3 |

The single biggest win: the march pass roughly halved.

**Optimisation 1c: over-relaxed steps.** Step 1.4 x the safe distance. If that
overshoots into rock, the field is nearly linear there, so one step back along
`d` (which is negative) lands on the surface. No visible artefacts in the test
scenes (thin features under 0.25 m cannot exist in this grid anyway).

| 720p, wall clock | open hills | cavern | ground level |
|---|---|---|---|
| after 1b | 24.6 ms | 23.6 ms | 39.3 ms |
| after 1c | 24.1 ms | 19.3 ms | 24.8 ms |
| march only after 1b | 12.2 | 16.1 | 27.3 |
| march only after 1c | 10.6 | 12.9 | 16.5 |

About 15-40% on top, most on the grazing view.

**Net effect of 1b + 1c** (before: wall clock one batch; after: GPU timestamps,
best of three, so slightly flattering): everything on at 720p 36.7 -> 18.6 ms
(-49%), 54.6 -> 18.0 ms (-67%), 66.7 -> 23.2 ms (-65%). March pass alone
25.2 -> 9.5, 43.0 -> 11.6, 50.0 -> 16.5 ms (-62% to -73%).

### 2. Shadow rays (35-40% of the frame)

*Symptom.* Turning shadows off took the open-hills frame from 18.6 to 11.7 ms at
720p, the cavern 18.0 to 13.7, ground level 23.2 to 16.7.

*Why.* Every lit pixel casts a second ray toward the sun, through the same
marcher. Most escape quickly, but a ray must cross the whole one-chunk shell
around the surface in small steps.

*What I did (and did not).* The shadow ray reuses the marcher, so it gained
optimisations 1a-1c for free. I tried capping it at 48 steps to save time: it
**left thin light slivers in long shadows** (rays that ran out of steps inside
the shell were counted as lit), and the saving was lost in the measurement noise.
The cap is now 128. **There is no clean "before/after" for shadows because the
noise was bigger than the effect.** Remaining ideas (not done): shadows at half
resolution, or a cached shadow map for static terrain.

### 3. One `write_texture` per chunk (edit upload)

*Symptom.* After a crater (r = 3 m) the bench measured **22.9 ms** of main-thread
time to upload 20 chunks.

*Why.* Each of the 20 `queue.write_texture` calls costs about 1 ms in the driver
regardless of its 4 KB payload.

**Optimisation 3a: one write over the bounding box of the dirty chunks.** Moves
a few more bytes in one call (falls back to per-chunk writes if the dirty chunks
are scattered). 22.9 -> **5.6 ms** per crater.

**Optimisation 3b: faster empty-space map rebuild.** The map is rebuilt after
every edit (an edit can turn an empty chunk into a surface chunk). The first
version bounds-checked every one of 26 neighbours for 16k cells; the rewrite
pads the array with a border and precomputes index offsets. 5.6 -> **2.4 ms**
per crater (rebuild alone: about 1.6 ms).

**Optimisation 3c: skip the rebuild when nothing changed.** If no chunk changed
between "holds a surface" and "does not", the rebuild and the upload are skipped
(digging repeatedly in one place).

Measured end state: **2.4-3.7 ms** per crater (varies with the machine), down
from 22.9 ms, about 6-9x. The CPU edit itself (`Grid::apply`) is 1.0-1.7 ms and
was not optimised.

### 4. Outline pass ran at screen resolution, so the resolution scale did not help it

*Symptom.* In the live window (a 1920 x 1080 window: see bottleneck 7) the post
pass took **8.6 ms** even when automatic resolution had dropped the march to 40%.

*Why.* The edge detection ran once per screen pixel, reading the (smaller)
G-buffer with nearest sampling, so lowering the render scale saved nothing there.

**Optimisation 4: three passes.** Ink at internal resolution into its own
texture, then a cheap present pass that upscales it bilinearly and adds grain,
vignette and crosshair. At 100% scale the cost is unchanged (ink + present about
2.5 ms vs 2.3-2.8 ms for the old single pass). At the same 1080p window and 40%
scale the post cost fell from **8.6 ms to 2.75 ms**, and it now scales with the
internal resolution. A small extra win inside the ink pass: the wide "pressed
pen" stroke (a second round of G-buffer reads) is only evaluated where it is
visible, about a third of the screen.

### 5. A weak GPU needs to adapt: automatic resolution

*Symptom.* At 100% the open-hills frame is 18.6 ms and ground level 23 ms at
720p: under 60 fps on this iGPU.

**Optimisation 5: GPU timestamp queries plus a resolution controller.** Each
pass is timed by the GPU itself (wall-clock timing cannot tell GPU time from
vsync waits and is very noisy here). The render scale follows
`scale * sqrt(target / measured)` (cost is mostly per pixel), in 5% steps, with a
+-8% dead band, a 12 ms target, and no adjustment during the first second and a
half (shader compilation made early frames look slow and the first controller
got stuck at 45%).

Result in the live window (1280 x 720, everything on): **60 fps at 65-70%
internal scale** (title bar: GPU 11-12.5 ms). For reference, the unoptimised
marcher at a true 1080p / 100% took 69-113 ms per frame (9-14 fps), and I did not
re-measure 1080p / 100% on the final build; at 1080p / 70% it is 21-30 ms.

### 6. Map generation took 19.5 seconds

*Symptom.* The window opened and froze for ~12-20 s. Found only because the
interactive window never reached "terrain uploaded" (the headless runs hid it:
nobody had timed `world::build`).

*Why.* The map function costs about 260 ns per voxel (a height field with noise,
a finite-difference slope, a dozen shape distances), and the conservative
distance estimate that `Grid::from_fn` requires left a thick shell of "undecided"
chunks, each needing all 4096 voxels evaluated: tens of millions of calls on one
thread.

| Step | Time |
|---|---|
| first version (single thread, noise evaluated per voxel) | **19.5 s** |
| + parallel chunks (`std::thread::scope`, 8 threads), height-field lookup table, all-rock chunks stored as uniform | 3.9 s |
| + 4x4x4 block shortcut inside undecided chunks (one evaluation decides 64 voxels) | 0.73 s |
| steady state | **0.6 s** (about 30x faster) |

A test checks the shortcuts change no voxel (`from_fn_shortcuts_change_no_voxel`).

### 7. Two environment traps that looked like performance problems

- **High-DPI scaling.** A `LogicalSize` 1280 x 720 window is 1920 x 1080 physical
  pixels at 150% display scaling: 2.25x the pixels. The first live runs were
  slower than the benchmark for exactly this reason. The window now asks for
  physical pixels and prints its size and scale factor at startup.
- **Vulkan backend crashes** (access violation inside `request_device`) on this
  Intel driver; DX12 works. Windows now defaults to DX12.

### 8. Worms (M2): looping over capsules per pixel cost 6-10 ms

Worms are drawn as analytic capsules inside the raymarcher (body, two eyes, gun,
rope: 5 per worm). A first version looped over all of them for every pixel, and
again in the terrain shadow ray. With four worms and one rope (17 capsules) on
screen, open hills at 1280 x 720:

| Version | frame | cost of the worms |
|---|---|---|
| no worms | 18.5 ms | |
| loop over all capsules, per pixel and per shadow ray | 28.2 ms | +9.8 ms |
| + a bounding-sphere early-out before the full capsule test | 26.6 ms | +8.0 ms |
| (experiment) shadow loop off | 25.1 ms | +6.4 ms |
| (experiment) loop body replaced by one comparison against a loaded value | 24.5 ms | +5.6 ms |
| **tile masks** (below), eyes and rope no longer cast shadows | **19.6 ms** | **+0.6 ms** |

The surprise is the fourth row: even with almost no arithmetic, the loop alone
cost ~5.6 ms, so the maths was never the problem. My reading (not verified in a
profiler) is that a dynamic loop over a dynamically indexed uniform array is
slow on this GPU, probably because each iteration waits for a load. The fix is
to avoid the loop instead of making it cheaper: the CPU projects each capsule's
bounding sphere onto a 32 x 18 grid of screen tiles and uploads one bitmask per
tile. A pixel reads its tile's mask and, almost everywhere, finds it zero. Only
the terrain shadow ray still loops, over the worm bodies alone. A test
(`tile_masks_cover_every_visible_point_of_a_capsule`) checks the masks never
miss a visible point, and another checks that the constants in the WGSL match
the Rust ones.

How these were measured: the first four rows come from separate benchmark runs
(each compares "everything" and "everything + worms" within one run, so the
difference is meaningful even though the absolute times drift). The last row
comes from the benchmark's new alternating mode (no worms, worms, no worms, ...,
best of five rounds each: 19.0 vs 19.6 ms), because the clock changes slowly and
a one-after-the-other comparison can land the change on one variant.

### 9. Background load looks like a performance bug

For a stretch of this session the same benchmark binary gave 18.8, 37, 44 and
61 ms for the same row (worst single frame 511 ms), and nothing in my code had
changed. The OS reported no throttling (`% Passive Limit` 100, 60 C), nothing
else was using the GPU, but a `backgrounddownload` process was using ~46% of the
CPU. The CPU and the iGPU share one ~15 W budget, so CPU work from anything
else takes clock speed from the GPU. After the power mode was switched to Best
performance the numbers were steady again (19.0 ms); I cannot say whether that
was the mode or the download ending. The CPU-side edit cost shows the same
effect: the r = 3 m crater measured 2.73, 1.02, 1.11 and 0.63 ms in four runs
of unchanged code. If the game feels uneven on this laptop, check for
background CPU load and the Windows power mode before suspecting the code.

### 10. Edits for weapons (M3): the faster `Grid::apply` was not needed

M3 was expected to need a faster `Grid::apply` (explosions, a tractor that edits
every few ticks). Measured first, release build, the bare sim (`sdf` bench):

| Edit | Cost |
|---|---|
| carve r = 1.5 m (a dig) | 0.12 ms |
| carve r = 3 m (a rocket) | 0.46 ms |
| carve r = 8 m (a Nova) | 4.2 ms |
| smooth add r = 0.8 m (a tractor lump) | 0.11 ms |

and in the viewer (`--bench`, CPU edit + texture upload): r = 3 m 0.7 + 1.5 ms,
r = 8 m 8.8 + 2.2 ms (the Nova craters overlap there, 9 m apart). Only the Nova
is a visible hitch: one frame per eight-second reload.

I tried the obvious fix, taking each chunk out of the grid once and editing it in
place instead of a bounds-checked lookup per voxel: 5-8% faster, so it was
reverted. The cost is the per-voxel distance maths (a square root each), not the
lookups. The idea that would pay is skipping chunks that lie entirely inside a
carve sphere, which become air without evaluating a voxel (about a third of a
Nova's chunks); not done, because one hitch per Nova does not justify it yet.

## What did not help (so nobody repeats it)

| Experiment | Result |
|---|---|
| Widen the stored distance band from 4 to 8 voxels (so a step can be up to 2 m, not 1 m) | no rendering gain within noise (e.g. open hills 18.8 -> 21.1 ms, ground 28.8 -> 26.1 ms); CPU edit cost **doubled** (0.98 -> 2.35 ms). Reverted. |
| Shadow rays capped at 48 steps | thin light slivers in long shadows; saving unmeasurable. 128 kept. |
| A per-capsule bounding-sphere early-out inside the per-pixel loop | 9.8 -> 8.0 ms worm cost: the loop itself was the cost (section 8). Kept, it is cheap, but the tile masks are what fixed it. |
| Rewrite `Grid::apply` to own each chunk once instead of a lookup per voxel (M3) | 4.2 -> 4.0 ms for r = 8 m, 0.46 -> 0.43 ms for r = 3 m. Reverted: the per-voxel maths is the cost (section 10). |
| Treat the benchmark's single averages as precise | the machine's clock varied 2x between identical runs, so the bench now reports the best of three batches. |

## Where the time still goes, and what is next

At 1280 x 720, 100% scale, current build (GPU ms, best of three):

| View | everything | no shadows | march only |
|---|---|---|---|
| open hills | 18.6 | 11.7 | 9.5 |
| cavern | 18.0 | 13.7 | 11.6 |
| ground level | 23.2 | 16.7 | 16.5 |

With four worms and a rope on screen the open-hills frame is 0.6 ms slower
(section 8).

Remaining ideas, with my estimate of the gain (not yet tried unless noted):

1. **Finer empty-space bricks (8 voxels instead of 16).** Rays hugging the ground
   spend most steps inside the one-chunk (4 m) shell around the surface; a
   thinner shell shortens that. Needs an incremental, region-limited distance
   transform so edits stay cheap. Estimate: ~25% of the march.
2. **Shadows at half resolution** (or reuse across frames). Up to ~20% of the
   frame, at the cost of softer or shimmering shadow edges.
3. **Low-resolution pre-pass for the ray start** (cone/beam marching). Helps
   views looking down; does little for grazing rays.
4. **Faster `Grid::apply` for big craters** (skip chunks wholly inside the
   shape): the Nova costs 4 ms in the sim and ~9 ms in the viewer (section 10).
   Small edits are already cheap (0.1-0.5 ms).
5. Not worth it so far: widening the distance band (tried, above).
