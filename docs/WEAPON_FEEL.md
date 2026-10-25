# Weapon feel: making twelve guns feel like twelve weapons

A design spec for the arsenal, written after M3. It answers one question: *how do
we make every weapon feel powerful?* It is about feel, not balance. Damage
numbers, cooldowns and range live in `sim/src/weapons.rs` and are the owner's to
tune; this document is about what a shot **looks, sounds and feels like**.

Read it as a menu. The "Order of work" section at the end says what I would do
first and how to check each step.

> **Status:** Phase 1 (sound) is implemented in `viewer/src/audio.rs`: a small
> oscillator/noise synth with the three layers, a voice per weapon, distance
> muffle and delay, pitch variance, and a Nova wind-up hum. The mix is
> deliberately hot (soft-clipped) and the heavy weapons have 2-4 s tails, so a
> Nova is a wall of sound rather than a click. Phase 2 (per-weapon impact) is
> implemented too: `Explosion` and `Hit` carry the weapon, and `viewer/src/feel.rs`
> is the single signature table for fire colour, debris, shock rings, hit sparks,
> camera shake, shove and lens punch. Phase 3 (victim reaction) is mostly in: hurt
> flash, flinch, Liero blood with terrain stains, and a health-pip bar. Phase 4
> (impact frame) is partly in: per-weapon camera punch and a screen flash with a
> chromatic split. Hitstop and phases 5-6 remain open.

## 1. The claim

In Ratchet & Clank a weapon does not feel powerful because it does a lot of
damage. It feels powerful because the game spends an enormous budget **proving
that the hit happened**, and it spends that budget *differently for every
weapon*.

Power is legible force, told in three beats:

1. **Anticipation** — before the shot. The spin-up, the glow, the clunk, the
   arc the reticle promises.
2. **The impact frame** — the moment of contact, carried on several channels at
   once. One channel is invisible; four is a cannon.
3. **Afterglow** — the world still reacting a second later. Smoke, cooling
   rock, a body that has not landed yet.

A weapon that skips any of the three reads as a stat line. The RYNO is not
remembered for its damage; it is remembered for the sound, the shake, and the
fact that the room is *gone* afterwards.

## 2. What Ratchet & Clank actually does

Six habits worth stealing, in order of how much they matter:

- **A weapon is a verb, not a stat block.** "The one that bounces around the
  room." "The one that makes them dance." "The one that sucks the ground out
  from under them." If a weapon cannot be described in one sentence without
  mentioning damage, it has no identity yet.
- **The world keeps the receipt.** The Groovitron changes enemy *state*, the
  Suck Cannon removes *terrain*, the Bouncer fills a room with visible
  projectiles you can watch chain. The strongest weapons change the situation
  and leave evidence, not just a health bar.
- **The impact frame is a stack.** A crunchy layered sound, a white flash on the
  victim, a knockback, a number, debris, a shake. Each channel is weak alone.
- **The victim loses.** Enemies flash, stagger, get launched, and burst into a
  reward. If the target does not visibly suffer, the player is shooting a wall.
- **Contrast is the currency.** The cheap pistol is dry and almost polite so
  that the RYNO can be a geological event. If everything is loud, nothing is.
- **Cost is a feature.** Ammo, reload, weight, a slow turn while carrying it.
  A weapon you can fire forever is a tool; a weapon you have to commit to is a
  decision.

Insomniac also gives weapons **personality** (Mr. Zurkon talks) and lets them
**grow** (use levels them up, and they get visually meaner). Both are cheap
identity: a name announced on swap, and a gun that gains a barrel.

## 3. What the arsenal already has

This is not a from-scratch job. The rarest parts are already built:

- **Terrain is permanent.** A rocket carves a crater and it *stays*. That is
  more proof of force than most shooters ever manage, and it means a miss is
  still a story. This is the single biggest advantage the game has.
- **The aim aid is a contract** (`World::aim_path`). Lobbed arcs, blast rings
  the exact width of the damage, a ricochet stub. A Nova's 8 m ring on the rock
  makes the *choice* feel heavy before the trigger is pulled. Most games have no
  anticipation channel at all.
- **Oversized guns with a real recoil curve** (`worms.rs`: `recoil_style`,
  `recoil_curve`). The gun kicks, overshoots, settles, and the face reacts.
- **Camera dynamics** (`camera.rs`): a distance-falloff shake, a spring kick,
  a landing dip, all applied to a drawing copy so they can never steer a shot.
- **Per-weapon colour and muzzle shape** (`effects.rs::flash`, `ammo`), plus
  smoke trails for the rocket, cluster and nova.
- **Knockback with lift**, which already gives rocket jumps: a skill expression
  that turns a weapon into a movement tool.
- **An event stream** (`Event::Shot|Beam|Whip|Explosion|Hit|Died|Teleported`)
  that is already the seam for effects and, later, sound.

The arsenal passes the one-sentence test almost completely. The problem is that
in the hand they **blur**, because they share one fireball, one hit spark and
one camera response.

## 4. Where the force leaks

| Leak | Evidence | Consequence |
|---|---|---|
| **No audio at all** | `viewer/Cargo.toml` has no audio crate; PLAN puts synthesised audio at M6 | Roughly half of impact is missing. A silent explosion is a puff of colour. |
| **One explosion recipe** | **Fixed in phase 2:** `Event::Explosion` carries the weapon, and `viewer/src/feel.rs` gives each one its own fire, debris, smoke, shock ring and camera response | A Nova and a bomblet are different pictures, not the same ball at two sizes. |
| **No hitstop, no punch** | `camera.rs`: `SHAKE_MAX` 0.12 m (drawn at ×0.66), `RECOIL_MAX` 0.07 m | The heaviest blast in the game moves the view less than a fist. |
| **The victim does not react** | `Anim` has `fire` but no hurt state (`worms.rs`); `Hit` is a flash and 5 sparks; `Died` is one white ball (`effects.rs`) | Hits land on a statue. No stagger, no launch, no reward burst. |
| **Afterglow is too short** | A fireball lives `0.25 s + 0.04 × radius`, smoke `0.8 s + 0.05 × radius` (`effects.rs`) | A cavern-sized blast is over before the dust settles. |
| **Reload is a timer** | Nova: 1 round, 8 s reload (`weapons.rs`) | The best ritual in the game has no ceremony. |
| **No confirmation layer** | Window title is the HUD; no hitmarker, no damage number, no kill chime, no weapon name | The game never tells the player they did well. |
| **The juice budget is flat** | Every shot gets a flash; the kick table is only `ROCKET 0.045 / NOVA 0.060 / _ 0.015` | The Blaster and the Nova are the same volume. |

## 5. The signatures

One row per weapon. "Verb" is the sentence a player should say. "Loudness" is
1-5 and is *relative*: it is the contrast budget, not a mixer setting. A weapon
at 2 must stay quiet even when it is the newest thing in the game.

| # | Weapon | Verb | Anticipation | Impact signature | Afterglow | Loudness |
|---|---|---|---|---|---|---|
| 0 | Blaster | the metronome | none: instant | dry brass tick, small hot flash, tiny kick | a wisp of smoke that is gone by the next shot | **2** |
| 1 | Rocket | the rocket jump | a short hiss as it leaves | a 3 m blast, a thump felt below the speakers, dirt thrown *up* | smoke hanging in the new crater for ~2 s, grit raining back down | **4** |
| 2 | Railgun | the line through the room | the filament snaps into place with a rising whine | a white-blue line, a crack that arrives *after* the line, debris at the wall | an afterimage fading over 0.5 s, a ringing ear | **4** |
| 3 | Bouncer | the billiard ball | none | ping on the first wall, pitch rising per bounce | each bounce leaves a short hot trail so you can read the chain | **3** |
| 4 | Bomb | the trap you leave | a blink and a beep while it sits; the beep accelerates | a deep, dirty boom; more ground than fire | a crater that smokes and a low rumble that outlasts the flash | **4** |
| 5 | Cluster | the mortar over the hill | its barrel is carried upward; the shell's fall whistle | twelve separate pops in sequence, not one boom | twelve small craters and one smoke bank | **4** |
| 6 | Swarm | the pack | a rising whine as the six launch | six staggered impacts over ~0.5 s | the last missile still curling in when the first has landed | **3** |
| 7 | Teleport | the swap | a hum that climbs | a snap, a violet ring, a hard cut in the world | the ringing ear; the swapped enemy visibly confused for a beat | **3** |
| 8 | Nova | the cavern eraser | a 1.2 s charge: the view narrows, a ring of light gathers and spins | white-out, a fireball that fills the crater, the ground rolling | a 3 s smoke column and a crater rim glowing orange, cooling to black | **5** |
| 9 | Block | the wall | none: it is instant | the rock *slams* into place, a dust ring, one hard thud | dust settling off the new face | **2** |
| 10 | Tractor | the bridge | a grinding hum while held | a continuous stream of lumps, each with a small impact | the new span, still dusty | **2** |
| 11 | Plasma Whip | the scar | a 0.14 s coil: the carried lash tightens and brightens, the arm draws back | the crack, a fourteen-metre horizontal sweep, a white starburst at the rock, motion ghosts | a long groove cooling orange → red → dark over ~2 s | **3** |
Two of these are already the best in the game and should be the template: the
**Whip** (its scar is shaped by the weapon: a horizontal groove burned by the
sweep) and the **Nova's aim ring** (the choice is heavy before the shot).

## 6. The systems the feel needs

### 6.1 A weapon id on the blast

`Event::Explosion` and `Event::Hit` now carry the weapon id. Every other effect
can therefore be authored per weapon instead of reusing one recipe: colour,
debris shape, ring, sound, shake and lens punch are all read from one table in
`viewer/src/feel.rs`.

### 6.2 The impact frame: hitstop and punch

Three separate ideas, cheapest first:

- **Punch, not wobble.** A directional camera kick *away* from the shot, plus a
  2-4° FOV punch that decays over ~120 ms. `camera.rs` already has the spring;
  it needs a stronger, per-weapon magnitude and a FOV channel. A Nova should
  visibly *shove* the view.
- **A white frame.** One frame (a few milliseconds at any refresh rate) of a
  bright full-screen tint on a heavy kill reads as a flashbulb and costs almost
  nothing. Gate it to your own kills. **Done:** `CameraFeel::burst` sets a 0..1
  flash that decays in ~75 ms; `fs_present` turns it into a hot white frame with
  a slight chromatic lens split. It is scaled per blast by the weapon's FOV
  punch and distance (a Nova whites the frame out, a bomblet blinks), and a hit
  on *you* or a kill of yours adds a short confirmation flash. Two extra texture
  taps run only while the flash is alive.
- **Hitstop.** 60-90 ms where the *view* stops while the sim keeps ticking. It
  is the strongest single trick in the book, and it is a multiplayer problem:
  through M4 (offline loopback) it is free; at M5 decide whether it is
  cosmetic-only on the local client or dropped. Do not put it in `sim`.

Rule: none of this may feed back into aim. `camera.rs` already draws from a copy
for exactly this reason; keep it that way.

### 6.3 The victim must react

The target is currently the least expressive thing on screen. Needs:

- **Hurt flash** — the worm's body goes white for 60-80 ms on a hit, scaled by
  damage. This is the cheapest "it connected" signal there is. **Done:** 0.10 s,
  scaled by damage, plus a squeezed-shut face and an open mouth.
- **Stagger** — a hit above a threshold interrupts the walk for a few ticks and
  tilts the body. Do it in the viewer from the `Hit` event, not in the sim (the
  sim stays authoritative and deterministic). **Done:** `Anim::hurt` drives a
  lean/roll/twist flinch for 0.4 s, strongest on the first frames.
- **Blood** — Liero's lesson: the hit does not need a number if it paints the
  room. **Done, and turned up:** `Event::Hit` carries the push direction;
  `effects.rs::blood` throws roughly five times the old spray — about 80
  self-lit droplets for a whip hit, with a handful of fat gobs — then raycasts
  stains around the feet and three on the wall behind. Droplets live ~1 s,
  stains 14 s. It is pure cosmetic particles, no collision and no sim state, and
  it draws from its own 128-primitive pool (see the budget note below).
- **Launch** — knockback already pushes. Make the *body* tumble: rotate the
  capsule chain along the velocity while airborne after a hit, and let the death
  throw the body 8-14 m/s instead of replacing it with a white ball.
- **Reward burst** — on death, a spray of shards or energy bolts from the body,
  plus the killer's confirmation cue. R&C's enemies burst because the burst *is*
  the reward. A death already gets a larger version of the blood fountain.

### 6.4 Afterglow and the receipt

The crater is the game's best asset; make it legible after the flash:

- Smoke life for an explosion should scale to 2-3 s for the Nova and ~1 s for a
  bomblet, not 0.8-1.2 s for every blast.
- A **cooling rim**: the grid already tracks which chunks an edit changed, so a
  short-lived hot band on freshly carved rock, fading over a few seconds, would
  read as heat. If that is too invasive, a ring of small glowing embers at the
  crater lip is enough.
- **Dust that settles**, not dust that vanishes. A slow, lit puff falling under
  gravity sells weight better than a bright burst.

### 6.5 Sound

This is the largest single win in the document and it is currently zero. The
architecture is already right: `sim` is pure, and `main.rs` says "sound will
listen too". Constraints from `CLAUDE.md`: **no external assets, synthesised
sound**, so this is a small synth (oscillator + noise + envelope + filter), not
a sample library.

Per shot, three layers:

1. **Transient** — a click or crack, 5-15 ms. This is what makes it read as a
   *shot* rather than a whoosh.
2. **Body** — the character of the weapon: a filtered noise burst for the
   blaster, a pitch-swept sine for the rocket, a metallic ring for the railgun.
3. **Tail** — reverb or a low rumble, 0.3-3 s depending on the weapon.

Plus: ±5% pitch variance per shot (otherwise repetition becomes machine-gun
irritation), distance filtering and delay (a distant Nova should arrive late and
muffled), and a sub-bass thump for the big three. A weapon's *silence* is also a
tool: the Blaster must not have a big tail.

### 6.6 Reload as ritual, and weapon weight

- The Nova's 8 s reload is the best ritual in the game and currently invisible.
  A spin-up ring, a rising hum, the HUD visibly empty. The reload is the cost;
  show it.
- **Weight**: carrying the Nova could slow the worm slightly, lower the camera,
  and deepen the footsteps. A committed weapon feels heavier than a free one.
- Swapping weapons should have a beat: a mechanical clunk, the name on screen.
  Right now the swap is silent and instantaneous.

### 6.7 The confirmation layer

- **Hitmarker** on the reticle, distinct for a kill (the classic is a small X
  that grows and fades).
- **Damage numbers** — arcade, but they are how players learn that a Nova is a
  one-shot and a blaster is ten. If that is too gamey, a segmented health bar on
  the target is the alternative. **Done (bar):** a wounded worm carries five
  chunky horizontal pips above its head — filled pips green → yellow → red,
  empty sockets left dark. It is vertical so it reads from every camera angle,
  and it only appears once there is damage to report.
- **Kill chime** and a brief "kills" flourish in the HUD.
- **Weapon name on swap**, large, for a beat.

### 6.8 The contrast budget

Enforce the Loudness column. Concretely:

- The Blaster's flash, kick, shake and sound stay at the bottom of every range.
- `SHAKE_MAX` currently caps everything at 0.12 m. Either raise the cap for the
  top end or scale the *distribution*: blaster 0.01, rocket 0.06, nova 0.20.
- Do not add the same smoke puff to every weapon. Silence and emptiness are part
  of the mix.
- **Mute the world, not the actors.** The terrain, sky and arena wall are pulled
  a third of the way to grey (`U.fx.y` in the march shader, `C` toggles it); the
  capsules (worms, guns, blood droplets, health pips) are not. Stains are paint
  on the ground, so they mute with it. One constant in `main.rs`
  (`WORLD_GRADE`) sets the amount. This is the cheapest contrast win available:
  it costs two arithmetic operations on environment pixels and makes every
  coloured effect read from twice as far.

## 7. Constraints

- **Determinism.** Nothing in `sim` may depend on the viewer, the clock, or
  audio. All of the above is triggered by existing events, so it stays a pure
  consumer. Hitstop and any time scaling are viewer-only.
- **Capsule budgets.** Gameplay gets 256 slots (`MAX_CAPSULES`): worms, the aim
  aid, projectiles, then bursts, dust and trails, so a busy fight trims the
  newest smoke first. The blood droplets get their own 128 slots
  (`MAX_BLOOD_CAPSULES`) and are packed after the gameplay pool; the renderer
  truncates each pool on its own, so cosmetic spray can never evict a rocket.
  The stains they leave are a separate 48-slot paint pool in the march shader
  (`MAX_STAINS`), so a crowded fight cannot erase a wound either. The shader's
  total capsule array is 384 primitives (12 tile-mask words), still inside the
  64 KB uniform limit. Within each pool, prefer fewer, bigger,
  longer-lived primitives over many small ones.
- **Frame cost.** The march pass is already ~16 ms on the target laptop. New
  effects must be cheap capsules and particles; no per-pixel work per explosion.
- **No external assets.** Synthesised audio and procedural shapes only.
- **Web later.** The mixer must not assume a native device; keep the synth
  behind a small trait so a WebAudio backend can be added at M6.
- **Not in scope.** Balance numbers, new weapons, HUD layout, match rules.

## 8. Order of work

Each phase should be checked in the game, not just in tests. `--fire id[,ticks]`
and `--weapon id` already take screenshots of any weapon at any moment, so most
of this is reviewable headless except sound.

| Phase | Work | Verify |
|---|---|---|
| **1** | **Sound.** A tiny synth + mixer; events drive it. Three layers, pitch variance, distance filter. | Fire each weapon in turn with eyes closed: can you name it? |
| **2** | **Weapon id on `Explosion`/`Hit`**, and a per-weapon effect table (colour, fireball, debris, ring, shake, kick). | `--fire 8` and `--fire 1` produce unmistakably different frames. |
| **3** | **Victim reaction**: hurt flash, stagger, blood, death launch + reward burst. | `--hurt 34`: the body flashes, flinches and bleeds. |
| **4** | **Impact frame**: punch table, FOV kick, white frame; hitstop offline only. | Rocket at your feet *feels* like a hit, not a nudge; `--flash 0.6` previews the white frame. |
| **5** | **Afterglow**: longer smoke, cooling crater rim, settling dust. | A Nova crater is still readable 3 s later from the spawn camera. |
| **6** | **Ritual + confirmation**: Nova spin-up, reload ceremony, weapon weight, hitmarker, damage numbers, name on swap. | Play one full fight and describe each kill afterwards. |

Phases 1-3 change the game more than 4-6. If only one ships, ship sound.

**Done so far.** Phase 2 landed in `viewer/src/feel.rs`: one signature table per
weapon drives the fire colour and size, debris (a bomb throws dirt, a rocket
throws sparks), the smoke that stays after the flash, a shock ring on the heavy
weapons, hit-spark colour, camera shake (a Nova is a 0.20 m shove), a
directional shove and a distance-scaled lens punch. A Nova and a bomblet are now
different pictures, and the audio uses the same weapon id instead of guessing
from the blast radius. Shock rings lie in the plane of the surface they hit, so
an explosion on a wall paints the wall. The Cluster is carried with its barrel
up and always launches on a lofted arc, even from a level crosshair.

The effects themselves are now billboards, not SDF solids: the inked picture is
HDR, a half-resolution bloom chain adds the light of every self-lit pixel, and a
filmic tone-map rolls the cores off to white. Fire, smoke, sparks and shockwaves
are camera-facing quads; the terrain, worms and guns are lit by up to eight
dynamic point lights from blasts and plasma. Blood stains are painted into the
terrain's own albedo in the march shader (a **stain disc**, see `Stain` in
`renderer.rs`), so the ground carries the colour and cannot hide it; crater
scorch is a **soot sphere** in the same shader, so the burn follows the curved
bowl a blast carves instead of needing a flat plane. A shot that ends on rock
emits `Event::Impact`, so a missed shot still sparks, scorches and ticks.

Explosions are a layered recipe: a white core, one big fire body plus seven
ragged lobes (noise-cut, so the silhouette is tongues of flame), rising embers,
rock chunks under gravity, a ground dust wave, a column of smoke and a faint
shockwave. The Blaster now bites a small hole out of rock, fires at 85 m/s on a
0.085 s cycle, and still keeps the driest flash and smallest kick in the
arsenal. The Bomb is a 5.5 m blast that leaves a 10 m crater. The Cluster throws
twelve bomblets, each with a 2.8 m blast, so one shell carpets an area.

The Nova's wind-up (`WeaponDef::windup`): 1.2 s, committed on
the press, cancelled by a weapon swap, drawn as a charge ring and a narrowing
view. And the per-weapon camera recoil (phase 4, partly): a shot now shoves the
view up, climbs the muzzle and punches the lens, instead of the old slide
straight back along the aim — a dolly, which is why the recoil was invisible.

The Plasma Whip is now a carried melee weapon, not a gun (phase 6, the ritual).
Its lash is part of the worm's own model and is never put away: at rest it
reaches about six metres forward, arcing and rippling like a live cable. The
wind-up only coils it for 0.14 s; the strike extends it to fourteen metres and
sweeps it flat across the front, then eases it back to the carried pose. It has
no magazine and holding fire keeps it swinging. The plasma is a thick glowing
core with an ink-dark edge, dark electric knots and additive halo billboards;
the sweep leaves fading motion ghosts, and a crack against rock throws a white
starburst. Where the sweep crosses rock, the sim samples the whole arc, stops
the lash at the first surface (no tunnelling), carves a shallow groove and emits
`Event::Scar`; the viewer cools that groove from orange through red to dark over
two seconds. The old crescent of tip puffs is gone: the lash is drawn from the
same `whip_path` curve the sim hits and scars.

Hits now react (phase 3, mostly): the victim flashes white, flinches off its
axis, sprays Liero-red blood that arcs out and stains the floor and walls, and a
wounded worm wears a five-pip health bar. The blood is purely cosmetic
(`effects.rs`), deterministic, and sits in its own capsule budget so a messy
fight trims stains before it trims rockets. `--hurt damage` takes a screenshot of
all of it at once.

The impact frame now has its white flash (phase 4, partly): explosions, damage
to the player and player kills fire a short screen flash with a chromatic split.
The world is also graded a third toward grey by default, so the weapons, blood
and health pips are the only strong colour; `C` A/Bs it in game and `--no-grade`
does the same for a screenshot.

## 9. Open questions for the owner

1. **How quiet should the Blaster stay?** The contrast only works if the
   baseline is genuinely quiet. Is a dry, small blaster acceptable, or does every
   weapon need to feel "big"?
2. **Hitstop online?** It is free offline (M4). At M5, cosmetic-only on the local
   client, or dropped?
3. **Wind-up? — answered.** Yes: the Nova charges for 1.2 s before it fires,
   and the charge is a commitment. Should the railgun get a shorter one (0.25 s)
   too, or stay instant so the two long-range answers feel different?
4. **Damage numbers or a health bar?** Numbers teach the arsenal fastest; a bar
   looks cleaner. Both is a HUD decision for M6.
5. **Weight?** Should the heavies slow the worm, or is mobility sacred?
6. **Death**: a launched ragdoll, or the current white flash plus a reward burst?
7. **Twelve weapons, or eight that are each unmistakable?** Every weapon must
   carry a verb. If one cannot, it is a stat row, and the slot is better spent
   elsewhere.
