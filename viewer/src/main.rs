// The viewer. M1: a destructible SDF world, raymarched on the GPU, drawn with a
// toon ramp and ink outlines. M2: a worm from the `sim` crate that you walk,
// jump, dig and swing around it. M3: eleven weapons, damage and respawning.
// (F switches to the M1 fly camera and brush.)
//
//   cargo run -p viewer --release                  the interactive viewer
//   cargo run -p viewer --release -- --shot a.png  render one frame to a PNG
//   cargo run -p viewer --release -- --worm-preview --shot worm.png  body sculpt sheet
//   cargo run -p viewer --release -- --bench       time the GPU per frame
//
// The split from CLAUDE.md: the rules live in `sim`. This file turns keys into
// `sim::Input`s, steps `sim::World` at a fixed 60 Hz and draws what it holds.

mod audio;
mod camera;
mod effects;
mod feel;
mod renderer;
mod shaders;
mod world;
mod worm_preview;
mod worms;

use audio::Audio;
use camera::{Camera, CameraFeel, Kick, FOLLOW_DIST};
use effects::Effects;
use feel::shot_kick;
use glam::{Vec2, Vec3};
use renderer::{FrameParams, Gpu, Offscreen, Renderer};
use sdf::{Grid, Op, Shape};
use sim::weapons::WHIP;
use sim::{dig_target, Event, Input, World, ARSENAL, DIG_RADIUS, DT, EYE_HEIGHT, MAX_HP, WEAPONS, WhipStyle};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

const HELP: &str = "\
Controls
  click the window  capture the mouse        Esc  release it (again to quit)
  W A S D           run (the worm turns), mouse  look and aim
  Space             jump (hold for height); press again in mid-air to double
  left mouse        fire (hold)              right mouse  shoot the rope / let go
  1-9 0 - =         pick a weapon, or the mouse wheel: Blaster Rocket Railgun Bouncer
                    Bomb Cluster Swarm Teleport Nova Block Tractor Plasma Whip
                    (the Nova charges for 1.2 s and the whip coils for 0.14 s
                    before they fire; a swap cancels either. The whip is melee:
                    hold fire to keep swinging)
  hold W into a wall  climb it; S climbs down; Space kicks off the wall
                     (low ledges and crater rims are climbed automatically;
                     a jump that reaches a higher lip grabs it)
  Left Shift        dig (hold); the ring shows the hole, straight ahead of the worm
  E / Q             reel the rope in (pulls you, so you can launch yourself) / out
  F                 fly camera with the M1 brush: W A S D Space Ctrl Shift fly,
                    left mouse carve, right mouse build, mouse wheel brush size
  F1 F2 F3          fixed render resolution 100% / 75% / 50%
  T                 automatic resolution (default): holds ~60 fps by scaling it
  G shadows   H ambient occlusion   O ink outlines   C colour grade   V vsync
  R reset the world
  M mute the sound (synthesised: every weapon has its own voice)";

struct Config {
    shot: Option<String>,
    /// `--worm-preview`: the SDF body on a studio sheet instead of the game.
    worm_preview: bool,
    bench: bool,
    size: (u32, u32),
    scale: f32,
    /// Adjust the render scale every half second to hold ~60 fps (viewer only).
    auto_res: bool,
    cam: Option<[f32; 5]>,
    edits: bool,
    /// `--worm x,z,yaw_deg`: a worm in the screenshot, standing there.
    worm: Option<[f32; 3]>,
    /// `--rope`: that worm has just shot its rope and swings from it.
    rope: bool,
    /// `--fire weapon_id[,ticks]`: that worm fires once and the picture is taken
    /// this many ticks later (default 10), so projectiles and blasts can be seen.
    fire: Option<(u8, u32)>,
    /// `--weapon id`: that worm simply holds this weapon, without firing, so a
    /// screenshot shows the gun itself instead of a muzzle flash.
    weapon: Option<u8>,
    /// `--hurt damage`: wound that worm before the shot, so the blood, the hurt
    /// flash and the health bar can be inspected without a second player.
    hurt: Option<f32>,
    /// `--flash 0..1`: fill the screenshot with an impact flash, for looking at
    /// the post effect without having to catch it in play.
    flash: Option<f32>,
    /// `--boom weapon,x,y,z[,ticks]`: carve a blast at that point and take the
    /// picture `ticks` later. A developer knob for looking at an explosion from
    /// a known angle instead of hunting for one in play.
    boom: Option<(u8, Vec3, u32)>,
    /// `--no-grade`: leave the environment colours alone (the interactive `C`
    /// key toggles the same grade).
    grade: bool,
    brush: bool,
    /// `--no-aim`: hide the weapon aim aid (arcs, blast rings, ricochet marks) in a screenshot.
    aim: bool,
    shadows: bool,
    ao: bool,
    outlines: bool,
}

fn parse_args() -> Config {
    let mut c = Config {
        shot: None,
        worm_preview: false,
        bench: false,
        size: (1280, 720),
        scale: 1.0,
        auto_res: true,
        cam: None,
        edits: false,
        worm: None,
        rope: false,
        fire: None,
        weapon: None,
        hurt: None,
        flash: None,
        boom: None,
        grade: true,
        brush: true,
        aim: true,
        shadows: true,
        ao: true,
        outlines: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| panic!("{name} needs a value"));
        match a.as_str() {
            "--shot" => c.shot = Some(value("--shot")),
            "--worm-preview" => c.worm_preview = true,
            "--bench" => c.bench = true,
            "--size" => {
                let v = value("--size");
                let (w, h) = v.split_once('x').expect("--size WxH, e.g. 1920x1080");
                c.size = (w.parse().expect("width"), h.parse().expect("height"));
            }
            "--scale" => {
                c.scale = value("--scale").parse().expect("--scale 0.5..1");
                c.auto_res = false; // an explicit scale means "keep it"
            }
            "--no-auto-res" => c.auto_res = false,
            "--cam" => {
                let v: Vec<f32> = value("--cam").split(',').map(|s| s.trim().parse().expect("number")).collect();
                c.cam = Some(v.try_into().expect("--cam x,y,z,yaw_deg,pitch_deg"));
            }
            "--edits" => c.edits = true,
            "--worm" => {
                let v: Vec<f32> = value("--worm").split(',').map(|s| s.trim().parse().expect("number")).collect();
                c.worm = Some(v.try_into().expect("--worm x,z,yaw_deg"));
            }
            "--rope" => c.rope = true,
            "--fire" => {
                let v = value("--fire");
                let (id, ticks) = v.split_once(',').unwrap_or((&v, "10"));
                c.fire = Some((id.trim().parse().expect("--fire weapon_id[,ticks]"), ticks.trim().parse().expect("ticks")));
            }
            "--weapon" => c.weapon = Some(value("--weapon").parse().expect("--weapon weapon_id")),
            "--hurt" => c.hurt = Some(value("--hurt").parse().expect("--hurt damage")),
            "--flash" => c.flash = Some(value("--flash").parse().expect("--flash 0..1")),
            "--boom" => {
                let v: Vec<f32> = value("--boom").split(',').map(|s| s.trim().parse().expect("number")).collect();
                let ticks = v.get(4).copied().unwrap_or(10.0) as u32;
                c.boom = Some((
                    v[0] as u8,
                    Vec3::new(v[1], v[2], v[3]),
                    ticks,
                ));
            }
            "--no-grade" => c.grade = false,
            "--no-brush" => c.brush = false,
            "--no-aim" => c.aim = false,
            "--no-shadows" => c.shadows = false,
            "--no-ao" => c.ao = false,
            "--no-outlines" => c.outlines = false,
            other => panic!("unknown argument {other}\n{HELP}"),
        }
    }
    c
}

/// The seed of the sim's random numbers (weapon spread, bomblets).
const SEED: u32 = 1;

/// How far the world is pulled towards grey by default. A third is enough to
/// stop the grass competing with the weapons without making the map look dead.
const WORLD_GRADE: f32 = 0.35;

/// The weapon a number-row key picks: 1 to 9 are the first nine, then 0, `-`
/// and `=`.
fn weapon_key(key: KeyCode) -> Option<u8> {
    Some(match key {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        KeyCode::Digit4 => 3,
        KeyCode::Digit5 => 4,
        KeyCode::Digit6 => 5,
        KeyCode::Digit7 => 6,
        KeyCode::Digit8 => 7,
        KeyCode::Digit9 => 8,
        KeyCode::Digit0 => 9,
        KeyCode::Minus => 10,
        KeyCode::Equal => 11,
        _ => return None,
    })
}

fn spawn_camera() -> Camera {
    Camera { pos: Vec3::new(64.0, 56.0, 28.0), yaw: 0.0, pitch: -0.42, fov_y: 70f32.to_radians() }
}

fn camera_from(c: &Config) -> Camera {
    match c.cam {
        Some([x, y, z, yaw, pitch]) => Camera {
            pos: Vec3::new(x, y, z),
            yaw: yaw.to_radians(),
            pitch: pitch.to_radians(),
            fov_y: 70f32.to_radians(),
        },
        None => spawn_camera(),
    }
}

fn in_world(grid: &Grid, p: Vec3) -> bool {
    let s = grid.size_metres();
    p.cmpge(Vec3::ZERO).all() && p.cmple(s).all()
}

/// The point the third-person camera orbits: just above the worm's eye.
/// `CameraFeel` smooths this point, so the view lags the body instead of being
/// welded to it (see `camera.rs`).
fn camera_target(feet: Vec3) -> Vec3 {
    feet + Vec3::Y * (EYE_HEIGHT + 0.2)
}

/// The impact speed of a worm that has just landed, if it fell hard enough to
/// matter. 4 m/s is about a 0.8 m drop; walking down a bump makes no dust.
/// Split out of `Viewer::landings` so the threshold is testable without a
/// window.
fn landing_impact(was_grounded: bool, on_ground: bool, fall: f32) -> Option<f32> {
    if was_grounded || !on_ground || fall > -4.0 {
        None
    } else {
        Some(-fall)
    }
}

/// The point the crosshair is on, if the camera is inside the world.
fn aim_point(grid: &Grid, cam: &Camera) -> Option<Vec3> {
    if !in_world(grid, cam.pos) {
        return None;
    }
    let dir = cam.forward();
    grid.raycast(cam.pos, dir, 100.0).map(|t| cam.pos + dir * t)
}

/// The direction from the worm's eye to whatever the crosshair is on.
///
/// The camera sits over the shoulder, so its centre ray is not the eye ray: a
/// shot along the raw look angles would land a shoulder-width to one side of
/// the reticle. Aim at the point under the crosshair instead. With nothing in
/// reach, the shot goes far along the camera ray.
///
/// Very close to the camera (a wall in your face after the camera has been
/// pulled in by a tunnel) the point under the crosshair is almost beside the
/// worm, and converging would swing the gun round. Fade the convergence out
/// over the first few metres instead; at that range the reticle is on the wall
/// either way.
fn crosshair_aim(grid: &Grid, cam: &Camera, eye: Vec3) -> Option<Vec3> {
    const NEAR: f32 = 1.0;
    const FULL: f32 = 3.0;
    const FAR: f32 = 100.0;
    let dir = cam.forward();
    let hit = aim_point(grid, cam).unwrap_or(cam.pos + dir * FAR);
    let blend = (((hit - cam.pos).length() - NEAR) / (FULL - NEAR)).clamp(0.0, 1.0);
    let to = (hit - eye).normalize_or_zero();
    if blend <= 1.0e-3 || to == Vec3::ZERO {
        return None;
    }
    Some(dir.lerp(to, blend).normalize_or_zero()).filter(|a| *a != Vec3::ZERO)
}

fn carve(grid: &mut Grid, at: Vec3, radius: f32) {
    grid.apply(Shape::Sphere { center: at, radius }, Op::Carve);
}

/// Build: new material pushed out along the wall, blended into it like clay.
fn build(grid: &mut Grid, at: Vec3, radius: f32) {
    let n = grid.gradient(at);
    grid.apply(Shape::Sphere { center: at + n * radius * 0.5, radius }, Op::Smooth(radius * 0.5));
}

/// A few craters and a blob, so `--shot --edits` shows what edits look like.
fn demo_edits(grid: &mut Grid) {
    for (x, z) in [(52.0, 22.0), (58.0, 26.0), (66.0, 24.0), (60.0, 32.0)] {
        if let Some(t) = grid.raycast(Vec3::new(x, 60.0, z), -Vec3::Y, 80.0) {
            carve(grid, Vec3::new(x, 60.0 - t, z), 3.5);
        }
    }
    if let Some(t) = grid.raycast(Vec3::new(76.0, 60.0, 24.0), -Vec3::Y, 80.0) {
        build(grid, Vec3::new(76.0, 60.0 - t, 24.0), 3.0);
    }
}

fn main() {
    println!("{HELP}\n");
    let cfg = parse_args();
    if cfg.worm_preview {
        let gpu = headless_gpu();
        worm_preview::run(&gpu, cfg.size, cfg.shot.as_deref().unwrap_or("worm_preview.png"));
    } else if cfg.bench {
        run_bench(&cfg);
    } else if let Some(path) = cfg.shot.clone() {
        run_shot(&cfg, &path);
    } else {
        run_window(cfg);
    }
}

// --- headless: screenshot and benchmark -----------------------------------

/// wgpu picks Vulkan first on Windows. The Intel Vulkan driver this was
/// developed on crashes inside `request_device`, while DX12 works, so Windows
/// defaults to DX12. `WGPU_BACKEND=vulkan` (or dx12, metal, gl) overrides it.
fn prefer_dx12_on_windows(mut desc: wgpu::InstanceDescriptor) -> wgpu::InstanceDescriptor {
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        desc.backends = wgpu::Backends::DX12;
    }
    desc
}

fn headless_gpu() -> Gpu {
    let desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(prefer_dx12_on_windows(desc));
    Gpu::new(&instance, None)
}

/// The world for a screenshot: the map, plus one worm if `--worm` asked for it,
/// settled on the ground (and swinging from its rope after `--rope`).
fn shot_world(cfg: &Config) -> (World, Effects) {
    let spawns: Vec<(f32, f32)> = cfg.worm.iter().map(|w| (w[0], w[1])).collect();
    let mut world = World::new(world::build(), &spawns, SEED);
    let mut effects = Effects::default();
    if let Some([_, _, yaw]) = cfg.worm {
        let look = Input { yaw: yaw.to_radians(), ..Input::default() };
        for _ in 0..60 {
            world.step(&[look]);
        }
        // The sim only turns the body when there is movement input, so a worm
        // that has just been dropped on the ground still faces +Z. The shot's
        // yaw is documented as where the worm faces, so point it there.
        world.worms[0].face_yaw = yaw.to_radians();
        // `--weapon` shows a held weapon with no shot, so its silhouette is clear.
        if let Some(id) = cfg.weapon {
            world.worms[0].current = (id as usize).min(world.worms[0].weapons.len() - 1);
        }
        if cfg.rope {
            // Shoot the rope up and forward, then let it swing for a third of a second.
            let shoot = Input { pitch: 0.7, rope: true, ..look };
            world.step(&[shoot]);
            for _ in 0..20 {
                world.step(&[Input { rope: false, ..shoot }]);
            }
        }
        if let Some((weapon, ticks)) = cfg.fire {
            world.drain_events();
            for t in 0..=ticks {
                world.step(&[Input { fire: t == 0, weapon, pitch: 0.05, ..look }]);
                effects.trails(&world.projectiles);
                for e in world.drain_events() {
                    effects.add(&e);
                    match e {
                        Event::Hit { pos, damage, dir, .. } => effects.blood(pos, dir, damage, &world.grid),
                        Event::Died { pos, .. } => effects.blood(pos, Vec3::Y, 120.0, &world.grid),
                        _ => {}
                    }
                }
                effects.update(DT);
            }
        }
    }
    if let Some((weapon, at, ticks)) = cfg.boom {
        let def = &WEAPONS[weapon as usize];
        if def.terrain > 0.0 {
            world.grid.apply(Shape::Sphere { center: at, radius: def.terrain }, Op::Carve);
        }
        effects.add(&Event::Explosion { weapon, pos: at, normal: Vec3::Y, radius: def.blast_radius });
        for _ in 0..ticks {
            effects.update(DT);
        }
    }
    for (i, w) in world.worms.iter().enumerate() {
        println!("worm {i} stands at {:.1}, {:.1}, {:.1}{}", w.pos.x, w.pos.y, w.pos.z, if w.rope.is_some() { " on its rope" } else { "" });
    }
    (world, effects)
}

fn run_shot(cfg: &Config, path: &str) {
    let gpu = headless_gpu();
    let (mut world, mut effects) = shot_world(cfg);
    let mut renderer = Renderer::new(&gpu, Offscreen::FORMAT, cfg.size, cfg.scale, &mut world.grid);
    if cfg.edits {
        demo_edits(&mut world.grid);
        println!("synced {} chunks", renderer.sync(&mut world.grid));
    }
    let cam = camera_from(cfg);
    let target = Offscreen::new(&gpu.device, cfg.size);
    let positions: Vec<Vec3> = world.worms.iter().map(|w| w.pos).collect();
    let mut anims: Vec<worms::Anim> = world.worms.iter().map(|_| worms::Anim::default()).collect();
    // A screenshot taken a few ticks after a shot should show the recoil at the
    // same point in its spring as the game would, not a worm frozen at rest.
    // A charge-up weapon has not fired yet if the picture is taken mid-wind-up.
    if let Some((weapon, ticks)) = cfg.fire {
        let elapsed = ticks as f32 * DT;
        let fired = elapsed >= WEAPONS[weapon as usize].windup;
        if fired {
            if let (Some(anim), Some(worm)) = (anims.first_mut(), world.worms.first()) {
                anim.fired();
                for _ in 0..ticks {
                    anim.update(worm, DT);
                }
                if weapon == WHIP {
                    // The strike lands after the draw-back; start its swing
                    // clock at the tick the sim would have fired it.
                    anim.struck_at(WhipStyle::Sweep, 0.0, false, elapsed - WEAPONS[WHIP as usize].windup);
                }
            }
        }
    }
    if let Some(damage) = cfg.hurt {
        // A developer convenience, like `--fire`: wound the worm so one PNG can
        // show the blood spray, the flinch and the health bar.
        if let Some(worm) = world.worms.first_mut() {
            worm.hp = (MAX_HP - damage).max(1.0);
        }
        if let (Some(worm), Some(anim)) = (world.worms.first(), anims.first_mut()) {
            let at = worm.pos;
            // Spray towards the camera: the wound is on the near side.
            let dir = -worm.aim_dir();
            effects.blood(at, dir, damage, &world.grid);
            // A sixth of a second in: the spray has left the body, without the
            // drops already falling out of frame.
            for _ in 0..10 {
                effects.update(DT);
            }
            anim.hurt(damage);
            anim.update(worm, 0.03);
        }
    }
    let (mut capsules, whip_glow, worm_sdfs) = worms::draw(&world.worms, &positions, &anims);
    for worm in &world.worms {
        capsules.extend(effects::charging(worm));
    }
    // The aim aid worm 0's weapon calls for, if it calls for one.
    if cfg.aim {
        if let Some(path) = world.aim_path(0) {
            capsules.extend(effects::trajectory(&path, &world.grid));
        }
    }
    capsules.extend(effects::projectiles(&world.projectiles, 1.0));
    capsules.extend(effects.core_capsules());
    let blood = effects.blood_capsules();
    let mut sprites = effects.sprites();
    sprites.glow.extend(whip_glow);
    sprites.sort_smoke(cam.pos);
    let params = FrameParams {
        camera: &cam,
        capsules: &capsules,
        worms: &worm_sdfs,
        blood: &blood,
        glow: &sprites.glow,
        smoke: &sprites.smoke,
        decals: &sprites.decals,
        lights: &sprites.lights,
        soot: &sprites.soot,
        stains: &sprites.stains,
        brush: if cfg.brush { aim_point(&world.grid, &cam).map(|p| (p, 3.0)) } else { None },
        time: 0.0,
        shadows: cfg.shadows,
        ao: cfg.ao,
        outlines: cfg.outlines,
        flash: cfg.flash.unwrap_or(0.0),
        grade: if cfg.grade { WORLD_GRADE } else { 0.0 },
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.render(&mut encoder, &target.view, &params);
    gpu.queue.submit([encoder.finish()]);
    renderer.end_frame();
    let pixels = target.read(&gpu);

    let file = std::io::BufWriter::new(std::fs::File::create(path).expect("cannot create the PNG"));
    let mut enc = png::Encoder::new(file, cfg.size.0, cfg.size.1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("png header").write_image_data(&pixels).expect("png data");
    println!("wrote {path} ({}x{})", cfg.size.0, cfg.size.1);
}

struct Timing {
    /// GPU ms per frame (march + post), best batch.
    total: f64,
    march: f64,
    post: f64,
    /// Slowest single frame of all batches.
    worst: f64,
}

/// Render frames and wait for the GPU after each. The times are the GPU's own
/// timestamps (falling back to wall-clock if the GPU has none). An integrated
/// GPU changes its clock with load, heat and power, so identical runs can differ
/// by 1.5x: run three batches and report the fastest batch average, which is
/// the GPU at its best clock, plus the slowest single frame.
fn time_frames(gpu: &Gpu, renderer: &mut Renderer, target: &Offscreen, params: &FrameParams, frames: usize) -> Timing {
    let mut one = || {
        let t = Instant::now();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        renderer.render(&mut encoder, &target.view, params);
        gpu.queue.submit([encoder.finish()]);
        renderer.end_frame();
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let wall = t.elapsed().as_secs_f64() * 1000.0;
        renderer.gpu_ms().map(|(m, p)| (m as f64, p as f64)).unwrap_or((wall, 0.0))
    };
    for _ in 0..5 {
        one(); // warm up: shader compilation, caches, clocks
    }
    let mut best = Timing { total: f64::MAX, march: 0.0, post: 0.0, worst: 0.0 };
    for _ in 0..3 {
        let (mut march, mut post) = (0.0, 0.0);
        for _ in 0..frames {
            let (m, p) = one();
            march += m;
            post += p;
            best.worst = best.worst.max(m + p);
        }
        let n = frames as f64;
        if (march + post) / n < best.total {
            best.total = (march + post) / n;
            best.march = march / n;
            best.post = post / n;
        }
    }
    best
}

fn run_bench(cfg: &Config) {
    let gpu = headless_gpu();
    let t = Instant::now();
    let mut grid = world::build();
    let n = grid.size_chunks();
    let surface_chunks = (0..n.x * n.y * n.z)
        .filter(|i| grid.chunk_has_surface(glam::IVec3::new(i % n.x, (i / n.x) % n.y, i / (n.x * n.y))))
        .count();
    println!(
        "map generation: {:.0} ms, {} of {} chunks hold surface",
        t.elapsed().as_secs_f64() * 1000.0,
        surface_chunks,
        n.x * n.y * n.z
    );
    let mut renderer = Renderer::new(&gpu, Offscreen::FORMAT, cfg.size, cfg.scale, &mut grid);
    let target = Offscreen::new(&gpu.device, cfg.size);
    println!("bench at {}x{}, render scale {:.0}%", cfg.size.0, cfg.size.1, cfg.scale * 100.0);

    // A camera standing 5 m above the ground at (x, z), found by shooting a ray down.
    let above_ground = |x: f32, z: f32, yaw: f32| {
        let t = grid.raycast(Vec3::new(x, 62.0, z), -Vec3::Y, 80.0).expect("ground");
        Camera { pos: Vec3::new(x, 62.0 - t + 5.0, z), yaw, pitch: -0.08, fov_y: 70f32.to_radians() }
    };
    let views = [
        ("open hills", spawn_camera()),
        ("cavern", Camera { pos: Vec3::new(64.0, 7.0, 46.0), yaw: 0.0, pitch: 0.03, fov_y: 70f32.to_radians() }),
        ("ground level", above_ground(30.0, 44.0, 0.8)),
    ];
    let variants: [(&str, bool, bool, bool); 4] = [
        ("everything", true, true, true),
        ("no shadows", false, true, true),
        ("no shadows, no ao", false, false, true),
        ("march only", false, false, false),
    ];
    let frames: usize = std::env::var("BENCH_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    println!("GPU time per frame from timestamp queries, best of 3 batches (ms)");
    println!("{:<14}{:<20}{:>9}{:>9}{:>9}{:>9}", "view", "variant", "total", "march", "post", "worst");
    for (name, cam) in &views {
        for (label, shadows, ao, outlines) in variants {
            let params = FrameParams {
                camera: cam,
                capsules: &[],
                worms: &[],
                blood: &[],
                glow: &[],
                smoke: &[],
                decals: &[],
                lights: &[],
                soot: &[],
                stains: &[],
                brush: None,
                time: 0.0,
                shadows,
                ao,
                outlines,
                flash: 0.0,
                grade: WORLD_GRADE,
            };
            let t = time_frames(&gpu, &mut renderer, &target, &params, frames);
            println!("{name:<14}{label:<20}{:>9.2}{:>9.2}{:>9.2}{:>9.2}", t.total, t.march, t.post, t.worst);
        }
    }

    // Worms on screen: four, each with a gun and eyes, one on a rope. Every
    // pixel tests its ray against all of these capsules.
    let mut worms: Vec<sim::Worm> = [(58.0, 40.0), (64.0, 44.0), (70.0, 40.0), (64.0, 52.0)]
        .iter()
        .map(|&(x, z)| {
            let t = grid.raycast(Vec3::new(x, 62.0, z), -Vec3::Y, 80.0).expect("ground");
            sim::Worm::new(Vec3::new(x, 62.0 - t, z))
        })
        .collect();
    worms[1].rope = Some(sim::Rope { anchor: Vec3::new(64.0, 60.0, 44.0), length: 10.0 });
    let positions: Vec<Vec3> = worms.iter().map(|w| w.pos).collect();
    let anims: Vec<worms::Anim> = worms.iter().map(|_| worms::Anim::default()).collect();
    let (capsules, _whip_glow, sdfs) = worms::draw(&worms, &positions, &anims);
    let with = FrameParams {
        camera: &views[0].1,
        capsules: &capsules,
        worms: &sdfs,
        blood: &[],
        glow: &[],
        smoke: &[],
        decals: &[],
        lights: &[],
        soot: &[],
        stains: &[],
        brush: None,
        time: 0.0,
        shadows: true,
        ao: true,
        outlines: true,
        flash: 0.0,
        grade: WORLD_GRADE,
    };
    let without = FrameParams { capsules: &[], worms: &[], ..with };
    // Alternate the two, best of five rounds each: a clock change on this GPU is
    // slow and large, and measured one after the other it would land on one variant.
    let (mut a, mut b) = (None::<Timing>, None::<Timing>);
    for _ in 0..5 {
        let x = time_frames(&gpu, &mut renderer, &target, &without, frames);
        let y = time_frames(&gpu, &mut renderer, &target, &with, frames);
        a = Some(match a { Some(p) if p.total <= x.total => p, _ => x });
        b = Some(match b { Some(p) if p.total <= y.total => p, _ => y });
    }
    for (label, t) in [("same view, no worms", a.unwrap()), ("everything + worms", b.unwrap())] {
        println!("{:<14}{:<20}{:>9.2}{:>9.2}{:>9.2}{:>9.2}", "open hills", label, t.total, t.march, t.post, t.worst);
    }

    // Edit cost: the CPU edit, the texture upload, and the frame after it.
    let cam = &views[0].1;
    let params = FrameParams {
        camera: cam,
        capsules: &[],
        worms: &[],
        blood: &[],
        glow: &[],
        smoke: &[],
        decals: &[],
        lights: &[],
        soot: &[],
        stains: &[],
        brush: None,
        time: 0.0,
        shadows: true,
        ao: true,
        outlines: true,
        flash: 0.0,
        grade: WORLD_GRADE,
    };
    // A rocket's crater (r = 3 m) and a Nova's (r = 8 m). The Nova ones are 9 m
    // apart, so they overlap and the later ones are a little cheaper than the first.
    let n = 100;
    for radius in [3.0f32, 8.0] {
        let (mut edit, mut sync, mut chunks) = (0.0, 0.0, 0usize);
        for i in 0..n {
            let x = 20.0 + (i % 10) as f32 * 9.0;
            let z = 20.0 + (i / 10) as f32 * 9.0;
            let Some(t) = grid.raycast(Vec3::new(x, 60.0, z), -Vec3::Y, 80.0) else { continue };
            let t0 = Instant::now();
            carve(&mut grid, Vec3::new(x, 60.0 - t, z), radius);
            edit += t0.elapsed().as_secs_f64() * 1000.0;
            let t1 = Instant::now();
            chunks += renderer.sync(&mut grid);
            sync += t1.elapsed().as_secs_f64() * 1000.0;
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            renderer.render(&mut encoder, &target.view, &params);
            gpu.queue.submit([encoder.finish()]);
            renderer.end_frame();
        }
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        println!(
            "r={radius} m crater x{n}: CPU edit {:.2} ms, upload {:.2} ms ({:.1} chunks) per edit",
            edit / n as f64,
            sync / n as f64,
            chunks as f64 / n as f64
        );
    }
    let t = Instant::now();
    for _ in 0..50 {
        std::hint::black_box(renderer::coarse_map(&grid));
    }
    println!("empty-space map rebuild: {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 50.0);
    renderer.set_scale(1.0);
}

// --- the interactive viewer ------------------------------------------------

struct Viewer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_cfg: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
    /// The game state: terrain, worms. Worm 0 is the player.
    world: World,
    /// Fixed timestep: real time not yet turned into sim ticks.
    acc: f32,
    /// Where each worm was before the last tick, for drawing between ticks.
    prev_pos: Vec<Vec3>,
    /// Per-worm animation clocks: gait phase, action age, blink and so on.
    anims: Vec<worms::Anim>,
    /// Environment desaturation, 0..1 (`C` toggles it for an A/B).
    grade: f32,
    seq: u32,
    /// Fireballs and beams, started by the sim's events.
    effects: Effects,
    /// The synth and its output stream. It listens to the same events; nothing
    /// in the sim or the effects knows it exists.
    audio: Audio,
    /// The camera's lag, landing dip, shake and shot kick. It moves the clean
    /// camera and adds its cosmetic offsets only when drawing (see `camera.rs`).
    feel: CameraFeel,
    /// Horizontal distance walked since the last footstep puff, per worm.
    step_dist: Vec<f32>,
    /// (was grounded, vertical speed) before a tick, per worm. Landing impact
    /// has to be measured inside the fixed-step loop: after `land` the sim has
    /// already flattened `vel.y`, so the renderer would only ever see zero.
    before: Vec<(bool, f32)>,
    /// The weapon worm 0 holds (an id, see `sim::weapons`).
    weapon: u8,
    /// The M1 mode: a free camera and the brush instead of the worm.
    fly: bool,
    camera: Camera,
    keys: HashSet<KeyCode>,
    lmb: bool,
    rmb: bool,
    grabbed: bool,
    brush_radius: f32,
    edit_cooldown: f32,
    shadows: bool,
    ao: bool,
    outlines: bool,
    vsync: bool,
    auto_res: bool,
    /// Smoothed GPU ms per frame (0 = no measurement yet at this scale).
    gpu_ms: f32,
    march_ms: f32,
    post_ms: f32,
    last_res_change: Instant,
    last_note: &'static str,
    started: Instant,
    last_frame: Instant,
    stats_since: Instant,
    frames: u32,
    sim_ms: f64,
    edit_ms: f64,
    sync_ms: f64,
    synced: usize,
}

impl Viewer {
    fn new(event_loop: &ActiveEventLoop, cfg: &Config) -> Viewer {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("liero3d M3")
                        // Physical pixels, not logical: on a display scaled to 150%+ a
                        // logical 1280 x 720 window is 2-4x the pixels and the frame
                        // time would depend on the user's desktop scaling.
                        .with_inner_size(PhysicalSize::new(cfg.size.0, cfg.size.1)),
                )
                .expect("cannot create a window"),
        );
        let desc = wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(event_loop.owned_display_handle()));
        let instance = wgpu::Instance::new(prefer_dx12_on_windows(desc));
        let surface = instance.create_surface(window.clone()).expect("cannot create a surface");
        let gpu = Gpu::new(&instance, Some(&surface));

        let adapter_caps = surface.get_capabilities(&gpu.adapter);
        // An sRGB surface format makes the GPU do the linear -> sRGB conversion
        // when the post pass writes, so all shading maths stays linear.
        let format = adapter_caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(adapter_caps.formats[0]);
        let size = window.inner_size();
        let surface_cfg = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: adapter_caps.alpha_modes[0],
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &surface_cfg);

        println!("window {}x{} px, display scale {:.2}", size.width, size.height, window.scale_factor());
        let audio = Audio::new();
        let mut world = World::new(world::build(), &world::SPAWNS, SEED);
        let renderer = Renderer::new(&gpu, format, (surface_cfg.width, surface_cfg.height), cfg.scale, &mut world.grid);
        let prev_pos = world.worms.iter().map(|w| w.pos).collect();
        let anims = vec![worms::Anim::default(); world.worms.len()];
        let worms = world.worms.len();
        // An explicit --cam means "look at this spot", which only the free camera can.
        let fly = cfg.cam.is_some();
        let mut camera = camera_from(cfg);
        if !fly {
            // Start already behind the worm; otherwise the first frame would
            // ease in from the spawn overview.
            camera.follow(camera_target(world.worms[0].pos), FOLLOW_DIST, &world.grid);
        }
        let now = Instant::now();
        Viewer {
            window,
            surface,
            surface_cfg,
            gpu,
            renderer,
            world,
            acc: 0.0,
            prev_pos,
            anims,
            seq: 0,
            effects: Effects::default(),
            audio,
            feel: CameraFeel::default(),
            grade: WORLD_GRADE,
            step_dist: vec![0.0; worms],
            before: vec![(true, 0.0); worms],
            weapon: 0,
            fly,
            camera,
            keys: HashSet::new(),
            lmb: false,
            rmb: false,
            grabbed: false,
            brush_radius: 3.0,
            edit_cooldown: 0.0,
            shadows: cfg.shadows,
            ao: cfg.ao,
            outlines: cfg.outlines,
            vsync: true,
            auto_res: cfg.auto_res,
            gpu_ms: 0.0,
            march_ms: 0.0,
            post_ms: 0.0,
            // In the future on purpose: no adjustment during the first 1.5 s, when
            // shader compilation and the first uploads make frames look slow.
            last_res_change: now + std::time::Duration::from_secs_f32(1.0),
            last_note: "",
            started: now,
            last_frame: now,
            stats_since: now,
            frames: 0,
            sim_ms: 0.0,
            edit_ms: 0.0,
            sync_ms: 0.0,
            synced: 0,
        }
    }

    fn set_grab(&mut self, grab: bool) {
        if grab {
            // Windows only supports Confined, other platforms only Locked.
            let ok = self.window.set_cursor_grab(CursorGrabMode::Locked).is_ok()
                || self.window.set_cursor_grab(CursorGrabMode::Confined).is_ok();
            self.grabbed = ok;
            self.window.set_cursor_visible(!ok);
        } else {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
            self.window.set_cursor_visible(true);
            self.grabbed = false;
            self.lmb = false;
            self.rmb = false;
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return; // minimised
        }
        self.surface_cfg.width = w;
        self.surface_cfg.height = h;
        self.surface.configure(&self.gpu.device, &self.surface_cfg);
        self.renderer.resize((w, h));
    }

    /// Print a message when the situation changes, not once per frame.
    fn note(&mut self, what: &'static str) {
        if self.last_note != what {
            println!("{what}");
            self.last_note = what;
        }
    }

    fn set_fixed_scale(&mut self, scale: f32) {
        self.auto_res = false;
        self.gpu_ms = 0.0;
        self.renderer.set_scale(scale);
    }

    /// Dynamic resolution. GPU cost is mostly per pixel, so cost ~ scale^2 and
    /// the scale that would hit the target is scale * sqrt(target / measured).
    /// A dead band of +-8% keeps it from hunting back and forth, and steps of 5%
    /// keep the number of buffer re-allocations low. It both lowers the scale
    /// when the frame is too slow and raises it again when there is room, so a
    /// slow moment (shader compile, a thermal dip) cannot leave it stuck low.
    fn adjust_resolution(&mut self) {
        const TARGET_MS: f32 = 12.0; // +8% band = 13 ms, leaving room in a 16.7 ms frame for the CPU and present
        if let Some((m, p)) = self.renderer.gpu_ms() {
            self.march_ms = m;
            self.post_ms = p;
            let total = m + p;
            self.gpu_ms = if self.gpu_ms == 0.0 { total } else { self.gpu_ms * 0.9 + total * 0.1 };
        }
        if !self.auto_res || self.gpu_ms == 0.0 || self.last_res_change.elapsed().as_secs_f32() < 0.5 {
            return;
        }
        let scale = self.renderer.scale();
        let wanted = (scale * (TARGET_MS / self.gpu_ms).sqrt()).clamp(0.4, 1.0);
        let over = self.gpu_ms > TARGET_MS * 1.08;
        let under = self.gpu_ms < TARGET_MS * 0.92 && scale < 1.0;
        let new = (wanted * 20.0).round() / 20.0;
        if (over || under) && (new - scale).abs() > 0.001 {
            self.renderer.set_scale(new);
            self.last_res_change = Instant::now();
            self.gpu_ms = 0.0; // the old average belongs to the old resolution
        }
    }

    fn key_pressed(&mut self, event_loop: &ActiveEventLoop, key: KeyCode) {
        match key {
            KeyCode::Escape => {
                if self.grabbed {
                    self.set_grab(false);
                } else {
                    event_loop.exit();
                }
            }
            // A fixed scale turns the automatic one off, so the choice sticks.
            KeyCode::F1 => self.set_fixed_scale(1.0),
            KeyCode::F2 => self.set_fixed_scale(0.75),
            KeyCode::F3 => self.set_fixed_scale(0.5),
            KeyCode::KeyT => {
                self.auto_res = !self.auto_res;
                self.gpu_ms = 0.0;
            }
            KeyCode::KeyG => self.shadows = !self.shadows,
            KeyCode::KeyH => self.ao = !self.ao,
            KeyCode::KeyO => self.outlines = !self.outlines,
            KeyCode::KeyC => self.grade = if self.grade > 0.0 { 0.0 } else { WORLD_GRADE },
            KeyCode::KeyM => {
                let muted = self.audio.toggle_mute();
                println!("sound {}", if muted { "muted" } else { "on" });
            }
            KeyCode::KeyV => {
                self.vsync = !self.vsync;
                self.surface_cfg.present_mode =
                    if self.vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
                self.surface.configure(&self.gpu.device, &self.surface_cfg);
            }
            KeyCode::KeyF => {
                self.fly = !self.fly;
                if !self.fly {
                    // Coming back from the fly camera: put the worm camera
                    // where it belongs instead of sweeping in from the sky.
                    let target = camera_target(self.world.worms[0].pos);
                    self.camera.follow(target, FOLLOW_DIST, &self.world.grid);
                    self.feel.snap(target);
                }
            }
            KeyCode::KeyR => {
                self.world = World::new(world::build(), &world::SPAWNS, SEED);
                self.effects = Effects::default();
                self.audio.stop_all();
                self.feel = CameraFeel::default();
                let n = self.world.worms.len();
                self.step_dist = vec![0.0; n];
                self.before = vec![(true, 0.0); n];
                self.prev_pos = self.world.worms.iter().map(|w| w.pos).collect();
                self.anims = vec![worms::Anim::default(); n];
                self.renderer = Renderer::new(
                    &self.gpu,
                    self.surface_cfg.format,
                    (self.surface_cfg.width, self.surface_cfg.height),
                    self.renderer.scale(),
                    &mut self.world.grid,
                );
            }
            // The number row picks the weapon with that id; `-` and `=` are the
            // last two.
            other => {
                if let Some(id) = weapon_key(other) {
                    self.weapon = id;
                }
            }
        }
    }

    /// What the keyboard and mouse ask worm 0 to do this tick.
    fn worm_input(&self) -> Input {
        let worm = &self.world.worms[0];
        if self.fly {
            // The free camera has the keys: the worm just stands and looks as before.
            return Input { yaw: worm.yaw, pitch: worm.pitch, weapon: self.weapon, ..Input::default() };
        }
        let held = |k| self.keys.contains(&k);
        let axis = |plus, minus| (if held(plus) { 1.0 } else { 0.0 }) - (if held(minus) { 1.0 } else { 0.0 });
        // The camera now lags the worm (see `camera.rs`), so aim from the camera
        // the player is actually looking through. Re-posing it at the worm's
        // current position would put the shot where the reticle is not.
        let aim_cam = self.camera;
        Input {
            seq: 0, // set per tick in `update`
            move_dir: Vec2::new(axis(KeyCode::KeyD, KeyCode::KeyA), axis(KeyCode::KeyW, KeyCode::KeyS)),
            yaw: self.camera.yaw,
            pitch: self.camera.pitch,
            // The camera is off the look ray, so tell the sim where the
            // crosshair actually is (see `crosshair_aim`).
            aim: crosshair_aim(&self.world.grid, &aim_cam, worm.eye()),
            jump: held(KeyCode::Space),
            dig: self.grabbed && held(KeyCode::ShiftLeft),
            rope: self.grabbed && self.rmb,
            reel: axis(KeyCode::KeyE, KeyCode::KeyQ),
            fire: self.grabbed && self.lmb,
            weapon: self.weapon,
        }
    }

    fn update(&mut self, dt: f32) {
        // Pattern: Fixed Timestep. The sim always advances in exact 1/60 s ticks,
        // however fast frames come: it adds the real time to an accumulator and
        // runs as many ticks as fit. (The frame time is capped at 0.1 s so a
        // long stall cannot trigger a burst of ticks.) What is left in `acc` is
        // how far we are into the next tick; `frame` uses it to draw smoothly.
        self.acc += dt;
        // Decay the screen flash here, not in `CameraFeel::update`: that one is
        // skipped in fly mode, and a flash must never get stuck on screen.
        self.feel.tick(dt);
        let mut input = self.worm_input();
        let t = Instant::now();
        let mut ticks = 0;
        while self.acc >= DT {
            self.prev_pos = self.world.worms.iter().map(|w| w.pos).collect();
            // Landing impact must be read before the tick: `movement::land`
            // clears `vel.y`, so afterwards every landing looks like zero.
            self.before.clear();
            self.before.extend(self.world.worms.iter().map(|w| (w.on_ground, w.vel.y)));
            input.seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            self.world.step(&[input]);
            // A trail is history: it has to be emitted per sim tick, not per
            // frame, or it would grow with the frame rate.
            self.effects.trails(&self.world.projectiles);
            self.landings();
            self.acc -= DT;
            ticks += 1;
        }
        if ticks > 0 {
            self.sim_ms = t.elapsed().as_secs_f64() * 1000.0;
        }
        // The sim's events start the effects (a fireball per explosion, a line per
        // beam) and the sounds. Both are pure consumers: neither can steer the
        // sim. The renderer needs no event for terrain edits: it asks the grid
        // which chunks changed.
        let (sound_right, _) = self.camera.right_up();
        for event in self.world.drain_events() {
            self.effects.add(&event);
            self.audio.play(&event, self.camera.pos, sound_right);
            // Camera dynamics: a blast pushes the view, a shot kicks it back,
            // a hit twitches it and a death is the biggest thump. All of it is
            // applied to the drawing copy only (see `camera.rs`), so none of it
            // can steer the aim.
            match event {
                Event::Explosion { weapon, pos, .. } => {
                    let away = (pos - self.camera.pos).length();
                    let falloff = (1.0 - away / 28.0).clamp(0.0, 1.0);
                    let look = feel::blast(weapon);
                    // Per-weapon weight, not one radius-shaped nudge: a Nova
                    // shoves the view, a bomblet does not.
                    self.feel.shake(look.shake * falloff * falloff);
                    self.feel.punch(look.fov * falloff);
                    // The flash tracks the lens punch: a Nova whites the frame
                    // out, a bomblet is only a blink.
                    self.feel.burst((look.fov / 4.5 * 0.65 * falloff).clamp(0.0, 0.85));
                    self.feel.kick(
                        &self.camera,
                        Kick {
                            up: look.push_up * falloff,
                            back: look.push_back * falloff,
                            pitch: look.push_pitch * falloff,
                            ..Kick::default()
                        },
                    );
                }
                Event::Shot { worm, weapon, .. } => {
                    // The gun recoils and the face reacts, for any worm.
                    if let Some(anim) = self.anims.get_mut(worm as usize) {
                        anim.fired();
                    }
                    if worm == 0 {
                        self.feel.kick(&self.camera, shot_kick(weapon));
                    }
                }
                Event::Whip { worm, from, phase, style, flip, .. } => {
                    // The lash is part of the worm's model, so the strike is an
                    // animation cue, not a spawned effect. The camera answers
                    // the crack, not a muzzle flash: a lateral shove with a
                    // small lens punch, timed to the swing.
                    if let Some(anim) = self.anims.get_mut(worm as usize) {
                        anim.struck(style, phase, flip);
                    }
                    if worm == 0 {
                        let away = (from - self.camera.pos).length();
                        let falloff = (1.0 - away / 24.0).clamp(0.0, 1.0);
                        self.feel.kick(&self.camera, shot_kick(WHIP));
                        self.feel.shake(0.026 * falloff);
                    }
                }
                Event::Hit { victim, weapon, pos, damage, dir } => {
                    // The victim flinches and flashes, and the hit paints the
                    // ground. Both are viewer state: the sim only says "this
                    // much damage, this way".
                    if let Some(anim) = self.anims.get_mut(victim as usize) {
                        anim.hurt(damage);
                    }
                    self.effects.blood(pos, dir, damage, &self.world.grid);
                    let away = (pos - self.camera.pos).length();
                    // The player being hit is always felt hardest.
                    let hurt = if victim == 0 { 1.6 } else { 1.0 };
                    let look = feel::hit(weapon);
                    self.feel.shake(look.shake * hurt * (1.0 - away / 18.0).clamp(0.0, 1.0));
                    // Taking a hit blinks the frame; hitting someone else does
                    // not, or a blaster duel would strobe.
                    if victim == 0 {
                        self.feel.burst((0.12 + look.shake * 6.0).min(0.45));
                    }
                }
                Event::Died { victim, killer, pos } => {
                    // A death is a fountain, not a puff: the body comes apart
                    // in front of the killer.
                    self.effects.blood(pos, Vec3::Y, 120.0, &self.world.grid);
                    let away = (pos - self.camera.pos).length();
                    let amount = if victim == 0 { 0.080 } else { 0.045 };
                    self.feel.shake(amount * (1.0 - away / 30.0).clamp(0.0, 1.0));
                    // A kill is the one confirmation the player should feel:
                    // a short white frame, not a full explosion white-out.
                    if killer == 0 && victim != 0 {
                        self.feel.burst(0.30);
                    } else if victim == 0 {
                        self.feel.burst(0.22);
                    }
                }
                _ => {}
            }
            // A worm the rules moved (a respawn, a teleport) must not be drawn
            // sliding there from where it stood, and the camera must not fly
            // across the map after it.
            if let Event::Respawned { worm } | Event::Teleported { worm, .. } = event {
                let at = self.world.worms[worm as usize].pos;
                self.prev_pos[worm as usize] = at;
                self.feel.snap(camera_target(at));
                if let Some(d) = self.step_dist.get_mut(worm as usize) {
                    *d = 0.0;
                }
                if let Some(anim) = self.anims.get_mut(worm as usize) {
                    *anim = worms::Anim::default();
                }
            }
        }
        self.effects.update(dt);

        if self.fly {
            self.fly_and_edit(dt);
        }
        let t = Instant::now();
        let n = self.renderer.sync(&mut self.world.grid);
        if n > 0 {
            self.sync_ms = t.elapsed().as_secs_f64() * 1000.0;
            self.synced = n;
        }
    }

    /// What the sim cannot tell the viewer afterwards: which worms touched down
    /// this tick, and how fast. Dust for every worm, a camera dip for the player
    /// only (the camera follows worm 0).
    fn landings(&mut self) {
        for i in 0..self.before.len().min(self.world.worms.len()) {
            let (was_grounded, fall) = self.before[i];
            let w = &self.world.worms[i];
            let Some(impact) = landing_impact(was_grounded, w.on_ground, fall) else {
                continue;
            };
            self.effects.dust(w.pos + Vec3::Y * 0.1, impact);
            if i == 0 {
                self.feel.land(impact);
            }
        }
    }

    /// The M1 mode: fly the camera and edit the terrain with the brush. The
    /// brush edits the grid directly, bypassing the sim: a developer tool.
    fn fly_and_edit(&mut self, dt: f32) {
        // Fly.
        let held = |k| self.keys.contains(&k);
        let (right, _) = self.camera.right_up();
        let fwd = self.camera.forward();
        let mut wish = Vec3::ZERO;
        if held(KeyCode::KeyW) {
            wish += fwd;
        }
        if held(KeyCode::KeyS) {
            wish -= fwd;
        }
        if held(KeyCode::KeyD) {
            wish += right;
        }
        if held(KeyCode::KeyA) {
            wish -= right;
        }
        if held(KeyCode::Space) {
            wish += Vec3::Y;
        }
        if held(KeyCode::ControlLeft) {
            wish -= Vec3::Y;
        }
        let speed = if held(KeyCode::ShiftLeft) { 40.0 } else { 14.0 };
        self.camera.pos += wish.normalize_or_zero() * speed * dt;
        // Stay inside the box: from above it, the rock below the top face would show.
        self.camera.pos.y = self.camera.pos.y.clamp(0.3, 63.0);

        // Edit. A button held down repeats at 20 Hz, so digging is a motion and
        // not a series of clicks.
        self.edit_cooldown -= dt;
        let aim = aim_point(&self.world.grid, &self.camera);
        if self.grabbed && (self.lmb || self.rmb) && self.edit_cooldown <= 0.0 {
            if let Some(p) = aim {
                let t = Instant::now();
                if self.lmb {
                    carve(&mut self.world.grid, p, self.brush_radius);
                } else {
                    build(&mut self.world.grid, p, self.brush_radius);
                }
                self.edit_ms = t.elapsed().as_secs_f64() * 1000.0;
                self.edit_cooldown = 0.05;
            }
        }
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.update(dt);
        if self.anims.len() != self.world.worms.len() {
            self.anims.resize(self.world.worms.len(), worms::Anim::default());
        }
        for (anim, worm) in self.anims.iter_mut().zip(&self.world.worms) {
            anim.update(worm, dt);
        }

        // Draw each worm between where it was a tick ago and where it is now.
        // Without this, on a display that is not exactly 60 Hz some frames would
        // show two ticks of movement and some none, and the worm would judder.
        let alpha = (self.acc / DT).clamp(0.0, 1.0);
        let draw_pos: Vec<Vec3> =
            self.world.worms.iter().zip(&self.prev_pos).map(|(w, &p)| p.lerp(w.pos, alpha)).collect();
        // Speed widens the view: the world streams past faster at the edges,
        // which is a cheap way to make fast movement FEEL fast. Horizontal
        // speed only: a straight-up jump must not read as a speed boost (the
        // sim's jump rule is the same, see `movement.rs`).
        const CALM_SPEED: f32 = 6.0; // no widening up to this speed
        let speed = if self.fly { 0.0 } else { Vec2::new(self.world.worms[0].vel.x, self.world.worms[0].vel.z).length() };
        let widen = ((speed - CALM_SPEED) / 12.0).clamp(0.0, 1.0);
        let target_fov = (70.0 + 14.0 * widen).to_radians();
        self.camera.fov_y += (target_fov - self.camera.fov_y) * (dt * 5.0).min(1.0);
        // A wind-up narrows the view, so the release has something to open out
        // from. Only the drawn copy changes; the clean fov is the aim's.
        let charge = if self.fly { 0.0 } else { self.world.worms[0].charge_fraction() };
        self.feel.set_charge(charge);
        self.audio.set_charge(charge);
        // The camera follows a smoothed point and adds its cosmetic offsets to
        // a copy. `self.camera` stays the clean camera used for aim.
        let render_camera = if self.fly {
            self.camera
        } else {
            self.feel.update(dt, &mut self.camera, camera_target(draw_pos[0]), FOLLOW_DIST, &self.world.grid);
            self.feel.render(self.camera)
        };

        // Footsteps: a puff every couple of metres, so running reads as
        // contact with the ground even though the worm has no legs.
        if self.step_dist.len() != self.world.worms.len() {
            self.step_dist.resize(self.world.worms.len(), 0.0);
        }
        if !self.fly {
            for i in 0..self.world.worms.len() {
                let w = &self.world.worms[i];
                let speed = Vec3::new(w.vel.x, 0.0, w.vel.z).length();
                if !w.alive || !w.on_ground || w.digging || speed < 4.0 {
                    self.step_dist[i] = 0.0;
                    continue;
                }
                self.step_dist[i] += speed * dt;
                if self.step_dist[i] >= 2.2 {
                    self.step_dist[i] = 0.0;
                    let at = w.pos;
                    let dir = Vec3::new(w.vel.x, 0.0, w.vel.z) / speed;
                    self.effects.footstep(at, dir);
                }
            }
        }

        let (mut capsules, whip_glow, worm_sdfs) = worms::draw(&self.world.worms, &draw_pos, &self.anims);
        // A charge is state, not an event: draw it for every worm, so a Nova
        // spin-up is a warning you can see coming.
        for worm in &self.world.worms {
            capsules.extend(effects::charging(worm));
        }
        // The aim aid comes before the effects so a busy fight trims smoke and
        // sparks, not the marker the player is looking at.
        if !self.fly {
            if let Some(path) = self.world.aim_path(0) {
                capsules.extend(effects::trajectory(&path, &self.world.grid));
            }
        }
        capsules.extend(effects::projectiles(&self.world.projectiles, alpha));
        capsules.extend(self.effects.core_capsules());
        // Cosmetic blood travels in its own pool, so it can never trim the
        // worms, the aim aid or a rocket.
        let blood = self.effects.blood_capsules();
        // Soft effects and surface marks: billboards and decals, sorted so alpha
        // smoke draws back-to-front.
        let mut sprites = self.effects.sprites();
        sprites.glow.extend(whip_glow);
        sprites.sort_smoke(render_camera.pos);
        // Show where a dig would land (or, flying, where the brush would).
        let brush = if self.fly {
            aim_point(&self.world.grid, &self.camera).map(|p| (p, self.brush_radius))
        } else {
            dig_target(&self.world.grid, &self.world.worms[0]).map(|p| (p, DIG_RADIUS))
        };

        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => (f, false),
            // Usable, but the surface would like to be reconfigured. Draw it
            // anyway (dropping it could repeat forever) and fix the size after.
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => (f, true),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.surface_cfg);
                self.note("surface outdated or lost, reconfigured");
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout => return self.note("surface timeout, frame skipped"),
            wgpu::CurrentSurfaceTexture::Occluded => return self.note("window occluded or minimised, not drawing"),
            wgpu::CurrentSurfaceTexture::Validation => return self.note("surface validation error, frame skipped"),
        };
        let view = frame.texture.create_view(&Default::default());
        let params = FrameParams {
            camera: &render_camera,
            capsules: &capsules,
            worms: &worm_sdfs,
            blood: &blood,
            glow: &sprites.glow,
            smoke: &sprites.smoke,
            decals: &sprites.decals,
            lights: &sprites.lights,
            soot: &sprites.soot,
            stains: &sprites.stains,
            brush,
            time: (now - self.started).as_secs_f32(),
            shadows: self.shadows,
            ao: self.ao,
            outlines: self.outlines,
            flash: if self.fly { 0.0 } else { self.feel.flash() },
            grade: self.grade,
        };
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.renderer.render(&mut encoder, &view, &params);
        self.gpu.queue.submit([encoder.finish()]);
        self.renderer.end_frame();
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        if suboptimal {
            let size = self.window.inner_size();
            if (size.width, size.height) != (self.surface_cfg.width, self.surface_cfg.height) {
                self.resize(size.width, size.height);
            }
        }

        self.adjust_resolution();

        // FPS in the title bar: no text rendering needed for a tech demo.
        self.frames += 1;
        let elapsed = self.stats_since.elapsed().as_secs_f64();
        if elapsed >= 0.5 {
            let on = |b| if b { "on" } else { "off" };
            // No text rendering yet (the HUD is M6): the title bar carries it.
            let me = &self.world.worms[0];
            let gun = &me.weapons[me.current];
            let def = &WEAPONS[gun.id as usize];
            let ammo = if !me.alive {
                format!("dead, back in {:.0} s", me.respawn_timer.max(0.0))
            } else if def.kind == sim::weapons::Kind::Whip {
                "melee".to_string()
            } else if gun.reload > 0.0 {
                format!("reloading {:.1} s", gun.reload)
            } else {
                format!("{}/{}", gun.ammo, def.mag)
            };
            let hud = format!(
                "hp {:.0} | {} {} | {} | kills {} deaths {}",
                me.hp,
                def.name,
                ammo,
                me.action.name(),
                me.kills,
                me.deaths
            );
            self.window.set_title(&format!(
                "liero3d M3 {} | {hud} | {:.0} fps ({:.1} ms) | gpu {:.1} ms (march {:.1} + post {:.1}) | sim {:.2} ms, edit {:.2} ms, upload {:.2} ms ({} chunks) | brush {:.1} m | res {:.0}%{} | shadows {} ao {} ink {} vsync {} sound {}",
                if self.fly { "fly" } else { "worm" },
                self.frames as f64 / elapsed,
                elapsed * 1000.0 / self.frames as f64,
                self.march_ms + self.post_ms,
                self.march_ms,
                self.post_ms,
                self.sim_ms,
                self.edit_ms,
                self.sync_ms,
                self.synced,
                self.brush_radius,
                self.renderer.scale() * 100.0,
                if self.auto_res { " auto" } else { "" },
                on(self.shadows),
                on(self.ao),
                on(self.outlines),
                on(self.vsync),
                if !self.audio.available() {
                    "unavailable"
                } else if self.audio.muted() {
                    "muted"
                } else {
                    "on"
                },
            ));
            self.frames = 0;
            self.stats_since = Instant::now();
        }
        let _ = event_loop;
    }
}

struct App {
    cfg: Config,
    viewer: Option<Viewer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.viewer.is_none() {
            self.viewer = Some(Viewer::new(event_loop, &self.cfg));
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(v) = self.viewer.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => v.resize(size.width, size.height),
            WindowEvent::Focused(false) => v.set_grab(false),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    match event.state {
                        ElementState::Pressed => {
                            v.keys.insert(code);
                            if !event.repeat {
                                v.key_pressed(event_loop, code);
                            }
                        }
                        ElementState::Released => {
                            v.keys.remove(&code);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                match button {
                    // The click that captures the mouse does not also dig.
                    MouseButton::Left if down && !v.grabbed => v.set_grab(true),
                    MouseButton::Left => v.lmb = down,
                    MouseButton::Right => v.rmb = down,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                if v.fly {
                    v.brush_radius = (v.brush_radius * 1.12f32.powf(lines)).clamp(0.5, 8.0);
                } else if lines != 0.0 {
                    // Scrolling up is the next weapon.
                    let n = ARSENAL.len() as i32;
                    let at = ARSENAL.iter().position(|&id| id == v.weapon).unwrap_or(0) as i32;
                    v.weapon = ARSENAL[(at + if lines > 0.0 { 1 } else { -1 }).rem_euclid(n) as usize];
                }
            }
            WindowEvent::RedrawRequested => v.frame(event_loop),
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        // Raw mouse deltas keep working when the cursor is pinned at the window edge.
        if let (Some(v), DeviceEvent::MouseMotion { delta }) = (self.viewer.as_mut(), event) {
            if v.grabbed {
                v.camera.look(delta.0 as f32, delta.1 as f32);
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(v) = &self.viewer {
            v.window.request_redraw();
        }
    }
}

fn run_window(cfg: Config) {
    let event_loop = EventLoop::new().expect("cannot create the event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut App { cfg, viewer: None }).expect("event loop failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec3;

    /// A 32 m cube of rock with an 8 m cave in the middle: walls 8 m out.
    fn cave() -> Grid {
        let mut grid = Grid::new(IVec3::new(8, 8, 8));
        grid.apply(Shape::Sphere { center: Vec3::splat(16.0), radius: 8.0 }, Op::Carve);
        grid
    }

    fn cam(pos: Vec3) -> Camera {
        Camera { pos, yaw: 0.0, pitch: 0.0, fov_y: 1.2 }
    }

    #[test]
    fn the_shot_converges_on_the_crosshair() {
        let grid = cave();
        let cam = cam(Vec3::new(16.0, 16.0, 10.0)); // 14 m from the far wall
        let eye = Vec3::new(15.25, 15.65, 14.0); // a shoulder left, a little below
        let hit = aim_point(&grid, &cam).expect("the wall ahead");
        assert!((hit - Vec3::new(16.0, 16.0, 24.0)).length() < 0.2, "crosshair on the wall: {hit:?}");

        let aim = crosshair_aim(&grid, &cam, eye).expect("converges at this range");
        let t = (hit.z - eye.z) / aim.z;
        let landed = eye + aim * t;
        assert!((landed - hit).length() < 0.05, "lands under the reticle: {landed:?} vs {hit:?}");
        // The old behaviour, shooting along the look angles, misses sideways.
        let straight = eye + cam.forward() * t;
        assert!((straight - hit).length() > 0.5, "the shoulder offset is real: {:?}", straight - hit);
    }

    #[test]
    fn the_aim_does_not_swing_round_at_point_blank_range() {
        // The camera has been pulled right up to the wall, so the point under
        // the crosshair is almost beside the worm: keep the look angles.
        let grid = cave();
        let cam = cam(Vec3::new(16.0, 16.0, 23.5)); // 0.5 m from the wall
        let eye = Vec3::new(15.25, 15.65, 14.0);
        assert!(crosshair_aim(&grid, &cam, eye).is_none());
    }

    #[test]
    fn only_a_real_fall_counts_as_a_landing() {
        assert_eq!(landing_impact(false, true, -9.0), Some(9.0));
        assert_eq!(landing_impact(true, true, -9.0), None, "already on the ground");
        assert_eq!(landing_impact(false, false, -9.0), None, "still in the air");
        assert_eq!(landing_impact(false, true, -2.0), None, "a step down, no dust");
    }
}
