# How the multiplayer works

This document follows one key press from your keyboard to a friend's screen,
then explains each idea you meet on the way. Every idea also has a comment next
to the code that uses it; the file names below tell you where.

## The big picture

```
 JOINER (a friend's game)              RELAY (small server)           HOST (a player's game, or the server)
 ────────────────────────              ────────────────────           ─────────────────────────────────────
 keyboard / mouse
   │ Input (60/s)
   └────────────────────────────────────> forwards bytes ───────────────> Session::on_relay → Host::handle(slot, input)
                                          (never decodes them)            │
                                                                          ▼  every 1/60 s
                                                                       Host::tick()  → World::step
                                                                          │ Snapshot (+ terrain edits)
                                                                          ▼ Throttle: every 2nd tick
   ┌──────────────────────────────────── fans out to everyone <──────── Session::after_tick
   ▼
 Replica (queue, plays 100 ms late)
   │
   ▼
 the raymarcher draws it
```

The player who hosts takes a shortcut: their own input and snapshots go through
an in-memory `Loopback` instead of the relay (`viewer/src/local.rs`). It speaks
the same messages, so offline play, hosting and joining all run the same
pipeline.

## Who does what (the three roles)

| Role | Runs | Knows about | Code |
|---|---|---|---|
| **Host** | the real `World`, at 60 ticks/s | slots, inputs, snapshots; *not* sockets | `host/src/lib.rs`, `host/src/session.rs` |
| **Relay** | nothing of the game | rooms and who is connected; *not* the rules | `server/src/rooms.rs` |
| **Client** | a copy (`Replica`), never `World::step` | what to draw; *not* the rules | `protocol/src/replica.rs`, `viewer/src/join.rs` |

**Authority.** Only the host changes the world. A client sends *wishes*
("I am pressing fire") and receives *facts* ("worm 2 has 40 hp"). It cannot say
"I hit him" or "I am at x = 5". That is what makes cheating hard and keeps
everyone's world identical.

**"Host is a role, not a place."** There are two ways to fill it, and the game
code is the same in both:

| | Host runs on | Friends reach it through | Good for |
|---|---|---|---|
| **HOST** in the menu | the hosting player's computer (`viewer/src/share.rs`) | relay: two network legs (friend → relay → host) | free; the host plays with zero lag |
| **SERVER ROOM** in the menu | the server (`server/src/matches.rs`) | relay: one leg to the server | nobody's tab or wifi matters; costs server CPU |

`host::Session` is the glue between a `Host` and a relay room. It owns no socket
and no clock (it takes decoded relay messages and *returns* the ones to send), so
the viewer and the server share it, and it is tested by plain function calls.

## The messages (`protocol/src/lib.rs`, `protocol/src/relay.rs`)

| Direction | Message | When | Size |
|---|---|---|---|
| client → host | `Input` | every sim tick | 34 bytes (measured) |
| client → host | `Ping` | once a second | tiny |
| client → host | `Resync` | when the terrain went wrong (see below) | tiny |
| client → host | `Rematch` | any time, from a seated player (see below) | tiny |
| host → client | `Welcome` + `Edits` slices | once, when you join (and after a resync or a rematch) | the edit log, in slices of 500 edits (~30 KB each) |
| host → client | `Snapshot` | 30 per second | ~3.4 KB with 4 worms (measured) |
| host → client | `Pong`, `Full` | answers | tiny |

Messages are turned into bytes with `bincode` right before sending and back
right after receiving (even on the in-memory loopback, so the encoding is
exercised on every run). Bytes from the outside are never trusted: oversized
messages are rejected before decoding and garbage is dropped, never fatal.

**Versions.** bincode has no field names, so a client and a host from different
builds would silently read each other's bytes as nonsense. The relay compares a
`PROTOCOL_VERSION` that the host states when it opens a room and the joiner states
when it knocks, and refuses a mismatch with a message ("RELOAD THE GAME").
Bump it whenever a message, `sdf::Shape` or the map generator changes.

## Ideas, in the order you meet them

### Tick
The sim advances in fixed steps of 1/60 s. The step counter (`tick`) labels
every snapshot, so a client knows *which moment* of the match it is looking at.
Fixed steps also mean the same inputs always give the same world.

### Command: the `Input` struct
Keyboard, touch and network clients all produce the same small `Input`
(`sim/src/input.rs`). The host does not care where one came from. Only the
*newest* input per player is kept: if two arrive between ticks, the older one is
already out of date. Inputs are *absolute* (what the player wants now: "fire is
held", "weapon 3"), never "press fire", so a lost or late packet is repaired by
the next one.

### Who is who: slots
The relay numbers connections (`PeerId`); the match numbers players
(`PlayerId`). `Session` keeps the table between them, and the slot always comes
from that table, never from the message, so a client cannot pretend to be
someone else. A seat nobody holds is a standing dummy until bots arrive.

### Late join and the `Welcome`
A friend can arrive mid-match. The host answers with a `Welcome`: the slot you
control, the world right now, and the **terrain edit log**. The map itself is
not sent: every client and the server run the same generator (`sim/src/map.rs`)
and get the same map, so a client builds the untouched map locally and replays
the host's edits on top. A match that has been dug up for ten minutes has tens
of thousands of edits, megabytes, so the wire cuts the log into slices
(`welcome_messages`) and the client puts it back together (`WelcomeBuilder`).

One subtlety, documented in `Session::after_tick`: a Welcome is only sent right
after a snapshot that went out, when the throttle holds no events back. Sent in
between, it would already contain the edits of the held-back ticks and the next
snapshot would carry them again: the newcomer would dig the same holes twice. A
test shifts the arrival across the whole window to guard this.

### Snapshots: state versus events
A snapshot has two kinds of content:

* **State** (positions, hp, projectiles): if one snapshot is lost or skipped,
  the next one replaces it. Skipping is harmless.
* **Events** (an explosion, a kill, a terrain edit): they happen once. If an
  explosion was in a skipped snapshot, the friend would never see it. So events
  are *queued* and ride along with the next snapshot that is really sent
  (`host/src/throttle.rs`, Pattern: Event Queue).

The terrain is not resent. A crater travels as one small `Edit` event (a shape
and an operation) and the client applies it to its own grid (Pattern: Dirty
Flag: the renderer re-uploads only the chunks that changed).

### Desync detection
Every snapshot carries `edits_total`, the number of edits the host has made so
far. The `Replica` counts the edits it has applied, and when it *shows* a
snapshot it compares the two. A difference means an edit was lost or doubled, so
the terrain no longer matches: the replica flags itself (`desynced`), the client
sends `Resync`, and the host answers with a fresh Welcome. Comparing a count costs
4 bytes; hashing the whole grid (`Grid::checksum`) was measured at ~70 ms, far
too slow to do every few seconds.

### The end of a match, and the rematch
The rules are the sim's (`sim/src/combat.rs`, `World::step`): the first worm to
`score_limit` kills (15) wins, a suicide costs a point, and once somebody has won
the world freezes (only the tick keeps counting, because snapshots are labelled
with it). Everyone learns it from `Snapshot::winner` and `Snapshot::score_limit`
(6 bytes in every snapshot, so a Welcome, which holds a snapshot, carries them to
a late joiner for free), and the `MatchWon` event makes the fanfare.

A new match is a `Rematch` message. **Any seated player may send it, at any time,
and it restarts the match for everyone** (the host ignores it from an empty seat or
an NPC). Any player, not just the room's owner, because a match the server hosts
has no owner. `Host` starts the next match (same map, same seats and rules, craters
gone, a fresh edit log, and new random spawn places) and bumps `match_number`. Everyone who owns a connection compares
that number with the last one they saw: `Session::after_tick` sends every friend
a new Welcome, and `LocalGame` builds the local player a new `Replica`. So a
rematch the host's own player asked for reaches the friends, and a friend's
reaches the host's screen, by the same path. Unlike a newcomer's, these Welcomes go
out at once: the throttle is emptied and that tick's snapshot is not sent, so
nothing held back from the old match can be applied to the new one.

### Snapshot rate and bandwidth
The sim runs at 60 Hz but friends get 30 snapshots per second. Measured with
`cargo run -p server --release --example tick_cost` (4 busy players):

| Snapshots per second | Host upload |
|---|---|
| 30 | ~99 KB/s |

The relay copies each snapshot to every friend, so the host uploads it once
however many friends there are. That is a lot for a home upload, and most of it
is the 15-weapon magazine table inside every worm (~450 bytes a worm). Sending
that table only when it changes is the obvious next saving.

### Interpolation (and why you see the past)
Snapshots arrive in steps and with jitter. Drawing the newest one the moment it
arrives would make everything jerk. Instead the `Replica` queues snapshots and
plays them back **6 ticks (100 ms) behind** the newest one, drawing the moment
*between* the two snapshots around its playhead (`protocol/src/replica.rs`,
Pattern: Double Buffer). A slightly late packet then arrives before it is
needed and nobody notices.

The cost: everything on a joiner's screen, including their own worm, is about
100 ms old, on top of the network delay.

### Latency
Press a key on a joiner. The time until you see the result is roughly:

```
 input travels to the host      ~ ping / 2
 waits for the next host tick   ~ 0 to 16 ms
 snapshot travels back          ~ ping / 2
 waits for the next snapshot    ~ 0 to 33 ms
 playback delay                 100 ms
```

With a 50 ms ping that is about 175 ms. The ping shown at the top of the screen
is the round trip to the *host* (a token the host sends straight back).

A player hosting has an unfair advantage: their own worm has no network delay at
all. There is no client-side prediction yet. If joining feels laggy, the usual
cure is to move your *own* worm locally and correct it when the host disagrees;
we measure first and add it only if needed.

### Why WebSocket and not UDP
A **browser may only open WebSockets** to a server (no raw UDP), and the web
build is a goal, so WebSocket is the one transport that works everywhere with a
single code path. It is TCP underneath: every message arrives, in order. That is
exactly what terrain edits need (a missed edit would leave one player's world
different for ever) and it means no lost-event or re-ordering code. The price is
**head-of-line blocking**: if one packet is lost, everything behind it waits for
the resend, which shows up as a short freeze, mostly on lossy links (mobile,
wifi, long distances). Servers on a good connection, close to the players, mostly
avoid it (that is one reason the relay is in Singapore).

What would change with UDP, and what would not:

* *Inputs and snapshots* are exactly what UDP suits: they are latest-wins, so a
  lost one is simply skipped. The protocol is already written this way (absolute
  inputs, full-state snapshots with a tick number), so these could move to an
  unreliable channel without changing their meaning.
* *Edits, the Welcome and the join handshake* need reliability. Games that use
  UDP build a small reliable channel on top (sequence numbers, acknowledgements,
  resends). That is real work and the part to be wary of.
* *In the browser* the ways out are WebRTC data channels (which can be
  unreliable, but need signalling and STUN/TURN servers) or WebTransport (QUIC;
  newer, less supported). *Render's web services only carry HTTP and WebSocket*,
  so UDP would also mean a different host (Fly.io or a plain VPS allow it).

So: WebSocket now, and swap only the socket layer if players on bad links
complain. `viewer/src/socket.rs` is that layer (four calls: `connect`, `state`,
`send`, `recv`).

The game loop never waits for the network. The socket only reports
`Connecting`, `Open` or `Closed`, and the frame loop reads whatever has arrived.
Natively it is a background thread around `tungstenite` (with `TCP_NODELAY`, so
small inputs are not held back by Nagle's algorithm, and TLS so it can reach a
`wss://` relay); in the browser it is the browser's own `WebSocket`.

### The relay
`server/src/rooms.rs` is deliberately dumb. It creates rooms with 4-character
codes (no look-alike letters), numbers the people in them, and forwards opaque
bytes: a joiner can only talk to the host, the host can talk to one person or to
everybody. The relay stamps the sender on each message so nobody can forge it.
If the host leaves, the room closes. It cannot cheat because it knows no rules,
and it cannot break when the game changes because it never decodes game
messages.

A server-hosted match (`server/src/matches.rs`) reuses all of that: the match is
a task that plays the part of a host connection, with in-process channels where a
socket would be, so the relay logic needs no special case.

## Limits you should know about

* **A hidden host tab freezes the match** (HOST in the menu). Browsers pause or
  slow background tabs, and the host's clock is the match's clock. The host must
  keep its tab visible, or use SERVER ROOM.
* **The host's connection is everyone's connection.** If the host's wifi
  stutters, the whole match stutters (SERVER ROOM avoids this).
* **A server room costs server CPU.** Measured on a fast desktop, a busy 4-player
  match averages 0.8 ms per tick (5% of a core) but has spikes (p99 11.6 ms, worst
  45 ms) when something large blows up terrain. See docs/DEPLOY.md before putting
  it on a plan with a fraction of a core.
* **No prediction, no rewind.** Firing at a moving target on a joiner is
  judged by the host's world, 100 ms or more after what the joiner saw.
* **Rooms live in the relay's memory.** If the relay restarts, rooms are gone.
  Run exactly one relay instance.
* **The edit log only grows.** A very long match makes the next late join
  megabytes; compacting it (or sending the grid) is not done yet.
* **No rate limiting or accounts.** Fine for friends; not for the open internet.

## Where to read the code

| To see... | Read |
|---|---|
| the messages | `protocol/src/lib.rs`, `protocol/src/relay.rs` |
| the authority | `host/src/lib.rs` |
| the host session (peers, slots, welcomes) | `host/src/session.rs` |
| snapshot thinning | `host/src/throttle.rs` |
| hosting a room from the game | `viewer/src/share.rs`, `viewer/src/local.rs` |
| joining | `viewer/src/join.rs` |
| the client's copy, interpolation and desync check | `protocol/src/replica.rs` |
| the relay | `server/src/lib.rs`, `server/src/rooms.rs` |
| matches the server hosts | `server/src/matches.rs` |
| the socket (native and browser) | `viewer/src/socket.rs` |
| the menu | `viewer/src/menu.rs`, `viewer/src/hud.rs` (`draw_menu`) |
| what a hosted match costs | `server/examples/tick_cost.rs` |
