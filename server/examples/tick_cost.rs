// How much does hosting a match cost the server? Run in release:
//
//     cargo run -p server --release --example tick_cost
//
// Four players run, fire, dig and swap weapons for thirty seconds of game time
// (the busy case: every weapon is exercised, craters are dug constantly), and
// the program reports how long each 1/60 s tick took on this machine, against
// the 16.7 ms it has to fit in, plus what that costs in bandwidth.
//
// Divide what you see by how much of a core the hosting plan gives you: a
// plan with a tenth of a core runs this ten times slower than this machine
// (if it has one core's worth of speed to start with).

use std::time::Instant;

use glam::Vec2;
use host::{Host, Session};
use protocol::{encode_host, FromRelay, HostMsg};
use sim::{map, Input, ARSENAL, DT};

fn main() {
    let started = Instant::now();
    let grid = map::build();
    println!("map generation: {:.0} ms (once per server run)", started.elapsed().as_secs_f64() * 1000.0);

    let mut host = Host::new(grid, &map::SPAWNS, 1);
    let mut session = Session::new();
    for peer in 1..=map::SPAWNS.len() as u8 {
        session.on_relay(&mut host, FromRelay::PeerJoined(peer));
    }

    let ticks = 30 * 60;
    let mut times = Vec::with_capacity(ticks);
    let mut snapshot_bytes = 0usize;
    let mut sent = 0usize;
    for tick in 0..ticks {
        // Four busy players: everyone runs in a circle of their own, fires all
        // the time and changes weapon every couple of seconds.
        for slot in 0..map::SPAWNS.len() {
            let turn = tick as f32 * 0.02 + slot as f32 * 1.7;
            let input = Input {
                seq: tick as u32,
                move_dir: Vec2::new(0.0, 1.0),
                yaw: turn,
                pitch: -0.25 + (turn * 0.5).sin() * 0.2,
                jump: tick % 90 == slot * 20,
                dig: tick % 240 < 40,
                fire: true,
                weapon: ARSENAL[(tick / 120 + slot) % ARSENAL.len()],
                ..Input::default()
            };
            host.handle(slot as u8, protocol::ClientMsg::Input(input));
        }

        let t = Instant::now();
        let snapshot = host.tick();
        let out = session.after_tick(&mut host, snapshot);
        times.push(t.elapsed().as_secs_f64() * 1000.0);

        for msg in out {
            if let protocol::ToRelay::Data { bytes, to: None } = msg {
                if matches!(protocol::decode_host(&bytes), Some(HostMsg::Snapshot(_))) {
                    snapshot_bytes += bytes.len();
                    sent += 1;
                }
            }
        }
        // Snapshots are what `encode_host` produces; the Welcomes at the start
        // are one-off and not counted.
        let _ = encode_host;
    }

    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let budget = DT as f64 * 1000.0;
    println!("{} players, {ticks} ticks (30 s of game time)", map::SPAWNS.len());
    println!(
        "tick: mean {mean:.2} ms, median {:.2}, p99 {:.2}, worst {:.2}   (budget {budget:.1} ms; mean = {:.0}% of one core of this machine)",
        times[times.len() / 2],
        times[times.len() * 99 / 100],
        times[times.len() - 1],
        mean / budget * 100.0
    );
    let per_snapshot = snapshot_bytes as f64 / sent.max(1) as f64;
    println!(
        "snapshot: {per_snapshot:.0} bytes, {sent} sent -> {:.0} KB/s uploaded by the host for everybody (the relay copies it to each of {} players)",
        per_snapshot * 30.0 / 1024.0,
        map::SPAWNS.len()
    );
}
