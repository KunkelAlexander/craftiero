# Neon Bomber-Liero — Plan (Rust + macroquad)

## Context
Build a fast, real-time, top-down "Bomberman meets Liero" arena shooter. It has a neon look (black background, glowing vector shapes), crazy long-range weapons, a grapple hook, destructible and buildable terrain, a pre-match weapon loadout, and online multiplayer with friends. No external assets: all visuals are procedural shapes plus a glow shader, and all sounds are synthesized. It must be hostable statically on GitHub Pages plus (at most) a cheap or free relay server. The project folder is empty, so this is greenfield.

## Decisions (confirmed with you)

**Tech**
| Topic | Decision | Why |
|---|---|---|
| Language/lib | **Rust + macroquad** | You mostly direct and I write the code. The web build is one `cargo` command, with no Emscripten. The game and server share a `protocol` crate. |
| Delivery | **Web (WASM) primary, native exe for fast iteration** | GH Pages hosts the static files. |
| Terrain | **Tile grid, 32 px tiles, ~40×22 map** | Easy to sync as tile diffs, easy to destroy and build. |
| Netcode | **Host-authoritative over a WebSocket relay, room codes, 2–6 players** | Simple and cheap. The host's browser runs the sim, so the host has a latency edge, which we'll note in the README. |
| Prediction | **None in v1.** Clients send inputs and interpolate snapshots. Add movement-only prediction only if it feels laggy. | Biggest netcode complexity, and we should measure first. |
| Relay hosting | **Render free tier for testing** (sleeps after ~15 min idle, ~30–60 s wake). Move to **Fly.io** (~$2–5/mo) if the game is fun. | You prefer not to spend money yet. The relay is a plain container, so moving is a config change. |
| Wake-up UX | The menu pings the relay on page load ("Connecting to server…"), and clients send a 30 s keep-alive ping. | Hides the cold start. Verify at the online milestone whether an open WebSocket keeps the instance awake (unsure, from memory). |

**Gameplay**
- **Movement:** free WASD plus mouse aim. The grid is for terrain and explosions only. Gamepad can come later.
- **Lethality:** 100 HP, 3 s respawn with brief invulnerability, **first to 15 kills**. Free-for-all.
- **Ammo:** per-weapon magazines with auto-reload. Strong weapons have small magazines and long reloads. No pickups in v1.
- **Self-damage:** 50% self-damage with full knockback, so explosions double as a movement tool.
- **Grapple hook:** RMB fires along the aim direction and anchors on the first solid or brick tile, then reels you in at ~2× walk speed. It has a ~1.5 s cooldown and can't hook players. It breaks if the anchor brick is destroyed, and you keep some momentum on release.
- **Building:** F places a neon block on the tile in your aim direction within ~3 tiles. You have **3 charges**, each refilling in ~4 s. Blocks have low HP, fade after ~15 s, and can't be placed on a tile holding a player or projectile.
- **Loadout:** chosen **once per match**, 5 weapons picked from 15, then everyone clicks Ready.
- **Bots/practice:** simple bots (walk toward the nearest player, shoot, occasional grapple) for solo testing and filling lobbies. Smarter AI is later.
- **Maps (3, ASCII in code):** "Grid" (Bomberman pillars and bricks), "Arena" (open centre, cover, grapple anchors), "Maze" (corridors with long railgun sightlines).
- **Controls:** WASD move, mouse aim, LMB fire, RMB grapple, Q/E or 1–5 switch weapon, F build, Space dash.

**v1 weapons (15, as data rows)**
1. Neon Blaster: fast, weak bolts.
2. Scatter: short-range shotgun.
3. Railgun: piercing instant beam, long reload, cuts bricks.
4. Rocket: medium explosion that digs a crater.
5. Bouncer: ricochet plasma.
6. Classic Bomb: timed, plus-shaped Bomberman blast.
7. Black Hole: gravity well that pulls in players and projectiles.
8. Cluster Bomb: bursts into small bombs.
9. Chain Lightning: arcs between nearby targets and walls.
10. Digger Beam: tunnels through bricks, no player damage.
11. Laser Sweep: ~1 s charge, then sweeps a long beam through an arc.
12. Teleport Gun: orb that swaps you with its landing spot.
13. Homing Swarm: ~8 tiny missiles that curve toward the nearest enemy and die on walls.
14. Time-Slow Bubble: area where everything moves at ~30% speed for ~4 s.
15. **Nova Cannon** (Ratchet & Clank-style mega-weapon, with an original name): a rocket with a very large blast radius that lights up the whole screen (flash plus glow surge) on detonation, with a very long reload and a 1–2 round magazine.

Cut from the earlier list: Flamethrower, Mine. They can return later.

## Coding rules and learning goals (new)
- **`CLAUDE.md` is created in M0** (none exists yet) and records these rules, so every later session follows them:
  1. Keep the code simple. Prefer plain structs, enums and functions over generics, traits and macros unless there's a clear payoff.
  2. Comment the *why* of design decisions (not the *what*) next to the code that implements them.
  3. When a pattern from *Game Programming Patterns* is used, name it in the comment (e.g. `// Pattern: Type Object`) and say why it fits here.
  4. For networking code, explain each concept where it appears (tick, authority, snapshot, interpolation, latency, reliable vs. unreliable) so the code teaches how online multiplayer works.
- **Online multiplayer learning path:** M2 is built in small, explained steps: (1) loopback `Net` (local, no network), (2) relay with rooms, (3) send inputs, (4) host snapshots, (5) client interpolation. I stop after each step so you can run it and see what it does. A short `docs/NETWORKING.md` records the architecture and how a message flows from key press to screen.

**Where *Game Programming Patterns* applies (and where it doesn't)**
| Pattern | Used for | Why it fits |
|---|---|---|
| **Game Loop** (fixed timestep) | `sim.rs` steps at 60 Hz; rendering runs at monitor rate | The same sim step must produce the same results on host and bots. Fixed `dt` keeps physics stable. |
| **Command** | Player `Input` struct (move, aim, buttons) | Bots, local keyboard and network clients all produce the same Input. That is what makes the sim network-ready. |
| **Type Object** | `WeaponDef` table | 15 weapons are rows of data, not 15 classes. Adding a weapon means adding a row. |
| **Flyweight** | Shared `WeaponDef`/colour data referenced by projectiles | Projectiles store only an id and their own state. |
| **State** | Player (Alive/Dead/Grappling/Reloading), game (Menu/Lobby/Loadout/Match/Results) | Replaces tangled `if` flags with explicit states. |
| **Object Pool** | Projectiles and particles in fixed-size arrays | Many short-lived objects with no allocation spikes. |
| **Event Queue / Observer** | The sim emits events (Explosion, Hit, Fire, TileDestroyed) | Audio, particles and the network all read the same events. The sim knows nothing about them, and it also gives the snapshot its event list. |
| **Double Buffer** | The two latest snapshots on a client | Interpolates between them so remote players move smoothly. |
| **Dirty Flag** | Tile changes (`changed_tiles` list) | Only changed tiles are sent over the network and redrawn. |
| **Spatial Partition** | The tile grid itself | Collisions only check nearby tiles. |
| **Update Method** | Each entity type has an `update(dt)` | Simple per-entity logic. It's a plain loop over structs, not a heavy component system. |
| *Not used:* Component/ECS, Service Locator, Bytecode, Data Locality | | Too much machinery for this size. If a part of the code starts needing them, I'll say so first. |

## Repo layout
```
CLAUDE.md                     # coding rules above (created in M0)
docs/NETWORKING.md            # how input -> host -> snapshot -> screen works (written in M2)
Cargo.toml                    # workspace: game, server, protocol
.github/workflows/deploy.yml  # cargo build wasm -> publish web/ to gh-pages
web/index.html                # black page, click-to-start (audio unlock), loads the wasm
game/src/main.rs              # window, state machine (Menu/Lobby/Loadout/Match/Results)
game/src/tilemap.rs           # EMPTY, SOLID, BRICK(hp), NEON_BLOCK(hp, decay); 3 maps
game/src/player.rs            # movement, aim, hp, respawn, grapple, build charges
game/src/weapons.rs           # data-driven WeaponDef table
game/src/projectile.rs, explosion.rs, bots.rs
game/src/sim.rs               # fixed 60 Hz step; "inputs in, state out"; the only code that mutates the world
game/src/neon.rs, shaders.rs  # glow pipeline; GLSL 100 strings, no asset files
game/src/particles.rs         # additive sparks, trails, shake, screen flash
game/src/audio.rs             # procedural sounds -> in-memory WAV -> macroquad Sound
game/src/net.rs               # Net trait: WebSocket impl + loopback impl (local/bots)
game/src/ui.rs               # menu, lobby, loadout, HUD
protocol/src/lib.rs           # Input, Snapshot, Event, relay messages (serde + bincode)
server/src/main.rs            # relay: create/join/forward, heartbeat
server/Dockerfile, render.yaml, fly.toml
```

## Key designs
- **Neon rendering:** scene into a `render_target` with additive thin lines plus a bright core, then bright-pass, downsample 2–4×, separable Gaussian blur, and additive composite with a vignette and an explosion-driven chromatic pulse. All shaders are GLSL 100 so they run on WebGL and native.
- **Audio:** synthesize oscillators (sine/square/saw/noise) with ADSR and sweeps, wrap each in a WAV header in memory, and load with `load_sound_from_bytes`. Audio starts after the first click.
- **Net-ready sim:** all game logic runs in `sim.rs`, driven only by `Input{seq, move, aim, buttons}` per player. Rendering and audio only read state and events. This is what makes the online slice cheap.
- **Netcode:** the host runs the sim at 60 Hz and broadcasts a `Snapshot` at ~30 Hz (players, projectiles, events). Tile changes go as reliable `(x, y, type)` diffs, and late joiners get the full map. Clients interpolate remote entities ~100 ms behind. Relay messages: `CREATE`→code, `JOIN code`, forwarded frames, `PEER_LEFT`. If the host leaves, the room ends.

## Milestones (reordered: online comes early)
1. **M0, toolchain, rules and deploy.** Install Rust and the wasm target. Write `CLAUDE.md`. A native window and a browser build both show a glowing triangle. The GH Action deploys to Pages.
2. **M1, core sim, local.** Tilemap, free movement, collisions, 3 weapons (Blaster, Rocket, Railgun), explosions, destructible bricks, simple bots. The sim is net-ready from the start.
3. **M2, thin online slice (taught step by step).** Relay server on Render, WebSocket `Net`, room-code lobby, 2 players, snapshots and interpolation, built in the five explained steps above, with `docs/NETWORKING.md`. You and a friend can playtest.
4. **M3, neon look and sound.** Glow pipeline, particles, shake, screen flash, synthesized sfx.
5. **M4, full feature set.** Grapple, block building, all 15 weapons, loadout screen, 3 maps, match flow, HUD, 2–6 players.
6. **M5, polish.** Balance from playtests, movement prediction if needed, gamepad, better bots, and a decision on moving the relay to Fly.io.

## Verification
- **Native:** `cargo run -p game --release`. Check glow, controls, weapons, map destruction and bots by hand.
- **Web:** `cargo build -p game --release --target wasm32-unknown-unknown`, serve `web/` with `python -m http.server`, and drive/screenshot it via the Playwright MCP tools. Confirm the shader compiles under WebGL and audio unlocks after a click.
- **Sim tests:** `cargo test` for tile destruction, damage and knockback maths, reload timing, and build-charge limits, all headless via the sim crate.
- **Relay:** `cargo run -p server` locally, with an integration test connecting two WebSocket clients to check create/join/forward. Then two browser tabs: one creates, the other joins by code, and inputs, projectiles and tile destruction stay in sync.
- **Deploy:** push to GitHub and confirm the Action publishes. Deploy the relay to Render, open the Pages URL, and play two browsers against each other over the internet. Time the cold-start wake-up and check the keep-alive.

## Risks and notes
- macroquad's WebSocket support (`quad-net`) is basic. If it falls short I'll write a small `web-sys` wrapper behind the same `Net` trait, isolated to `net.rs`.
- Keep shaders to GLSL 100 and test the glow at lower resolution on integrated GPUs.
- The host has a latency edge, and clients pay one relay hop. This is acceptable for friends.
- Render's free-tier terms may have changed since my knowledge, so I'll check the current docs when we reach M2.


## Addendum (M1)
- **Owner requirement: the architecture must allow a later move to a central (dedicated) server.** Done by making the rules a headless `sim` crate and treating "host" as a role. In M2 the host logic lives in one struct (`Host`: inputs by player id -> `World::step` -> snapshots) that does not know whether it runs in a browser or on a server; the relay then simply becomes "relay + Host".
- `game/src/sim.rs` became the `sim/` crate. Game-side code is split by concern: `view` (camera, letterbox, glow), `controls` (devices -> `Input`), `effects` (cosmetic state built from events), `draw` (reads the world), `main` (loop).

## Addendum (M2 step 1)
- Two more crates besides `sim`: `protocol` (messages, byte encoding, client `Replica`) and `host` (the authority as a message-in/message-out struct). The relay `server` crate comes in step 2 and does not link `host`.
- Offline play now runs the whole online pipeline over an in-memory `Loopback` (Input -> Host -> Snapshot -> Replica -> draw), so going online only swaps the wire.
- Steps re-sliced: step 1 already carries inputs and snapshots (original steps 3 and 4) over loopback. Remaining: 2 relay with rooms, 3 host/join over the relay (play with a friend), 4 snapshot rate and bandwidth, 5 client interpolation, then `docs/NETWORKING.md` and deploy.

## Addendum (M2 step 2)
- `server` crate = relay: rooms by 4-letter code, peer ids stamped by the relay, host leaving closes the room, joiners can only talk to the host. Room logic (`rooms.rs`) is plain synchronous code with unit tests; `lib.rs` is the thin axum WebSocket shell; `tests/e2e.rs` uses real sockets.
- quad-net rejected (panics on failed connect, no close detection). Own wrapper: `game/src/socket.rs` (tungstenite thread natively, browser WebSocket via `web/net.js`).
- Game now has a menu (P offline, H host, J join by code) and a lobby screen that shows the room code, friends count and joiner->relay->host->relay->joiner round-trip time. Relay URL: `RELAY_URL` env at build time, default `ws://127.0.0.1:9001/ws`.
- Native client has no TLS (browsers handle wss themselves); a native build can only talk to a `ws://` relay.

## Addendum (M2 step 3)
- Hosting is "offline play plus a room": `LocalGame` gets an optional `Share` (`game/src/share.rs`, the host session: relay peer -> player slot table, Welcome to arrivals, `Host::handle` for their inputs, Pong for pings, snapshots broadcast with `Data { to: None }`). The host plays immediately and friends drop in mid-match, so there is no separate lobby for the host. R = rematch (friends get a fresh Welcome).
- Joining is `game/src/join.rs` (pure client: Replica + inputs at most once per tick + ping). The old lobby screen was removed. `present.rs` draws a Replica for both paths.
- New `HostMsg::Full` so a joiner is told when all 4 slots are taken.
- Known limit: a hosting browser tab that is hidden is paused by the browser, so friends see the match freeze (a joiner measured a 35 s ping). The host must keep the tab visible, or the host role moves to a dedicated server.

## Addendum (M2 step 4)
- `Host::tick()` now returns a `Snapshot` every tick (no more `SNAPSHOT_EVERY` inside the host). The rate belongs to each *link*: the host's own player keeps 60/s over the in-memory loopback, friends get 30/s (`SEND_EVERY = 2` in `share.rs`).
- `host::Throttle` thins the stream and moves the events of skipped ticks into the next snapshot that goes out (state can be skipped, events cannot). `Share` resets it when no friend is listening and on rematch, so stale events never reach a newcomer or a new match.
- `Host::join()` no longer drains events (its Welcome snapshot carries none), which fixes the step-3 caveat.
- Measured with a unit test (4 players): 24 KB/s at 60 snapshots/s, 12 KB/s at 30. A test keeps this under budget. Bandwidth is ~87 bytes per player per snapshot, mostly floats that change every tick, so quantising or delta-coding would save little for the complexity; deliberately not done. Per-friend rates were also skipped: they would end the single broadcast upload.
- Consequence: friends now see the world in 30 Hz steps. Step 5 (interpolation) smooths that.

## Addendum (M2 step 5)
- `Replica` is now a playback buffer (protocol/src/replica.rs): snapshots queue up, a fractional `playhead` runs `delay` ticks (6 = 100 ms) behind the newest one and gently speeds up or slows down to stay there (`CATCH_UP`), or jumps after a long stall (hidden tab). `advance(seconds)` is called once per frame by `Presenter::show`.
- What is drawn: players blend between the two snapshots around the playhead (angles the short way, no blend across a teleport); projectiles have no id, so they are moved on from the older snapshot along their velocity; events and tile changes are released when the playhead passes their snapshot, so a brick breaks exactly when its explosion shows.
- Delay 0 = no interpolation: the local host player keeps zero extra lag. Joiners start with 6; the I key toggles it so the effect can be seen.
- Cost: the joiner sees everything, including their own player, ~100 ms in the past on top of the network round trip. Cutting that for the own player is what client-side prediction would do (M5, only if it feels bad).
- Not done: a fake-lag/jitter option. On localhost there is no jitter, so the buffer's real job (absorbing late packets) is only unit-tested until we play over the internet.

## Addendum (M2 step 6: docs and deploy files)
- `docs/NETWORKING.md` (how a key press reaches a friend's screen, every concept, limits) and `docs/DEPLOY.md` (relay on Render, game on Pages, Fly alternative) written.
- Deploy files, all unrun on real hosts: `server/Dockerfile` (build from repo root, LTO off for the build, same Debian release in both stages), `.dockerignore`, `render.yaml` (free plan, health check `/`), `fly.toml` (one machine only: rooms live in memory). The Pages workflow now reads `RELAY_URL` from a repo variable (unset = local relay).
- `Share` pings the relay every 20 s: a host alone in a room would otherwise send nothing, and idle WebSockets get cut / free Render services sleep after 15 min without incoming messages (checked against Render's docs, Oct 2026).
- Not verifiable here: the Docker build (no Docker installed; the exact cargo command inside it was run natively with `--locked` and LTO off and works) and the real deploy (needs a GitHub remote and a Render account: user actions).
- Known limit: the native client has no TLS, so only the browser build can use a `wss://` relay.

## Addendum (mobile)
- The window is resizable and the picture takes the screen's shape (no letterbox). `view::Layout` divides it up: the HUD keeps a fixed size and stacks into 1-3 rows by width; the arena camera shows the whole map if tiles stay >= 24 px (`MIN_TILE_PX`), else zooms in and follows the local player. Different screens see different amounts of the arena (accepted; the camera is presentation only).
- Touch: `game/src/touch.rs`, floating twin sticks (left moves, right aims and fires past 30 % push), HUD card taps switch weapons, a corner X stands in for Esc, a tap after a match stands in for R. Everything ends in the same `Input`, so sim/protocol/host are unchanged.
- Buttons (room-code keypad, armory CLEAR/GO) are immediate-mode (`draw::button`) and work by mouse and by tap (macroquad turns taps into clicks).
- Phones play in landscape. `web/device.js` reports a coarse pointer (phone/tablet; a touchscreen laptop counts as desktop until touched) and on the first tap requests fullscreen + landscape lock (Android; iPhone Safari cannot). Portrait on a phone shows a "turn your device" screen. Phones start in touch mode with ghost MOVE / AIM + FIRE sticks; the menu gets PLAY/HOST/JOIN buttons in its bottom strip; the armory keeps two columns from 600 px; the HUD stays one row from 820 px (status line moves to the arena's top left).
- Camera: minimum tile size 32 CSS px on phones, 24 on desktop (`MIN_TILE_PX_PHONE/DESKTOP` in view.rs); when zoomed, a smoothed look-ahead leads the view 25 % towards the aim direction (only along axes that scroll).

## Addendum (match setup and maps)
- PLAY and HOST go menu -> **match setup** (`game/src/setup.rs`) -> armory -> match; JOIN skips the setup, because the host's rules are the match's. Settings are remembered between matches like the loadout.
- Settings: map, bots (1-3, offline only; a room for friends always has 4 seats, bots hold the empty ones), first to 5/10/15/25. `protocol::Rules { map, score_limit }` travels in the `Welcome`; the map is sent as an index into `sim::tilemap::MAPS` (Type Object, like `WeaponDef`), the replica builds that map and lays the current tiles over it. `World::score_limit` replaces the constant (which stays as the default).
- Maps: **Grid**; **Anthill** (earth everywhere, ant tunnels, rock layers so digging opens galleries instead of flattening the map); **Pinball** (open, no bricks, single-pillar bumpers, staircase slingshots, a diamond in the middle; cover is what you build). Test: every map has >= 4 spawns, all reachable from each other without digging, and bots score on every map.
