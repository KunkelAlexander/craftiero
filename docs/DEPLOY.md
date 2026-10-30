# Putting the game online

Two pieces go online, and they are independent:

| Piece | What it is | Where | Cost |
|---|---|---|---|
| **The game** | static files in `web/` (HTML, JS, the `.wasm`) | GitHub Pages | free |
| **The relay** | `server` crate: rooms by code, forwards bytes, and can host matches itself | Render (Singapore) or Fly.io | free, $7 a month for more CPU, or a few dollars on Fly |

The game is told where the relay lives by one build-time setting, `RELAY_URL`
(`viewer/src/net.rs`). Without it the game uses `ws://127.0.0.1:9001/ws`, which
only works on your own computer. Desktop builds can also override it at run time
with `--relay URL`.

Order matters: relay first (you need its address), then the game.

## 1. Put the code on GitHub

The repository has commits but no remote yet. Create an empty repository on
GitHub, then:

```
git remote add origin https://github.com/<you>/<repo>.git
git push -u origin main
```

(The workflow in `.github/workflows/deploy.yml` runs on pushes to `main`. It
will fail until steps 2 and 3 are done; that is expected.)

## 2. The relay on Render

1. Render dashboard → **New → Blueprint** → choose the repository.
   `render.yaml` describes the service (name `craftiero-relay`, **region
   Singapore**, free plan); accept it.
2. The first build compiles Rust from scratch and takes several minutes.
3. When it is live, open `https://<name>.onrender.com/`. It should say
   `craftiero relay`.
4. The relay's WebSocket address is the same host with `wss://` and `/ws`:
   `wss://<name>.onrender.com/ws`.

**The region cannot be changed afterwards.** To move, create a new service (and
change `RELAY_URL`). Singapore is a compromise for players in Asia and Europe;
the ping the game shows (top of the screen while playing online) tells you how it
turned out.

What to expect from the free plan (0.1 CPU, 512 MB; checked against Render's
[compute plans](https://render.com/docs/compute-plans), October 2026):

* The service **falls asleep after 15 minutes without incoming traffic** and
  takes about a minute to wake on the next connection. The game shows
  "CONNECTING TO THE RELAY..." meanwhile. A host sends a small ping every 20 s,
  so the relay stays awake while someone is hosting.
* 750 free instance hours per month: enough for one service running all month.
* Render may restart the service at any time. Open rooms are lost when it does.
* Only HTTP and WebSocket are carried (no UDP): see "Why WebSocket and not UDP"
  in `docs/NETWORKING.md`.

### Server rooms: does the free plan cope?

The menu has two ways to host. **HOST** runs the match on the hosting player's
computer and the relay only copies bytes (almost no CPU). **SERVER ROOM** runs
the match on the server. That needs CPU, and a tenth of a core is not much.

Measured on a fast desktop with `cargo run -p server --release --example
tick_cost` (4 players running, firing and digging non-stop):

| | |
|---|---|
| map generation | ~3 s, once per server run (a match then copies it) |
| a tick (budget 16.7 ms) | mean 0.8 ms, median 0.05 ms, **p99 11.6 ms, worst 45 ms** |
| bandwidth | ~99 KB/s upload from the match, copied to each player |

The *mean* is small (about 5% of a core of this machine), but the *spikes* are
single ticks where something large blows up terrain, and on a plan with a tenth
of a core they would be many times longer: visible hitches in the middle of a
fight. So treat free-plan server rooms as a trial, and run the same example's
numbers past your plan before inviting people. If it stutters, either change
`plan: free` to `plan: starter` in `render.yaml` (0.5 CPU, $7 a month) or use
HOST. The server refuses more than 4 matches at once (`MAX_MATCHES` in
`server/src/matches.rs`), because each one also holds its own copy of the
terrain in memory; that number has not been measured against 512 MB, so lower it
if the service restarts under load. By arithmetic, not measurement: the map's
distance data is about 27 MB, a match holds two copies of it (the one being dug
up and the untouched one a rematch starts from), and the server keeps one more
to build matches from, so four matches are roughly 250 MB before anything else.

### Alternative: Fly.io

`fly.toml` is ready (Singapore, 512 MB). Fly has no free tier for new accounts
but it has no cold start. From the repo root:

```
fly launch --no-deploy --copy-config
fly deploy --ha=false
```

`--ha=false` matters: rooms live in one server's memory, so two machines would
be two separate worlds.

## 3. The game on GitHub Pages

1. In the GitHub repository: **Settings → Secrets and variables → Actions →
   Variables → New repository variable**:
   `RELAY_URL` = `wss://<name>.onrender.com/ws`
2. **Settings → Pages → Source: GitHub Actions**.
3. Push to `main` (or re-run the workflow). When it finishes, the game is at
   `https://<you>.github.io/<repo>/`.

Browsers only allow `wss://` (not `ws://`) from an `https://` page, which is why
the relay address must start with `wss://`. Render provides the certificate.

## 4. Try it

Open the Pages address in two browsers (or two computers). In one choose **HOST**
and note the room code across the top; in the other choose **JOIN** and tap the
code. Or choose **SERVER ROOM**: the code appears the same way, and friends join
it the same way. With HOST the hosting tab must stay visible.

## Running it all on your own computer

```
cargo run -p server --release                        # the relay, port 9001 (or $PORT)
cargo run -p viewer --release -- --host              # a window that hosts a room
cargo run -p viewer --release -- --join ABCD         # another that joins it
```

Add `--relay ws://host:port/ws` to either window to use another relay. The
native client has TLS built in (so it can use a `wss://` relay), but that has
not yet been tried against the real deployed one.

## Building the web version locally against the deployed relay

```
$env:RELAY_URL = "wss://<name>.onrender.com/ws"
powershell -File web/build.ps1 -Serve
```

Then open http://127.0.0.1:8080. Changing `RELAY_URL` rebuilds the game
automatically.

## Checking the relay by hand

* `curl https://<name>.onrender.com/` → `craftiero relay`
* Run it locally: `cargo run -p server --release` (port 9001, or `$PORT`).
* The Dockerfile has **not** been built on the machine that wrote it (no Docker
  there). The exact `cargo build` command inside it was run and works. If the
  first Render build fails, the build log will say why; the usual suspects are a
  Rust or base-image version, or the build machine running out of memory.
