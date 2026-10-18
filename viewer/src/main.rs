// The viewer. M1: a destructible SDF world, raymarched on the GPU, drawn with a
// toon ramp and ink outlines. M2: a worm from the `sim` crate that you walk,
// jump, dig and swing around it. (F switches to the M1 fly camera and brush.)
//
//   cargo run -p viewer --release                  the interactive viewer
//   cargo run -p viewer --release -- --shot a.png  render one frame to a PNG
//   cargo run -p viewer --release -- --bench       time the GPU per frame
//
// The split from CLAUDE.md: the rules live in `sim`. This file turns keys into
// `sim::Input`s, steps `sim::World` at a fixed 60 Hz and draws what it holds.

mod camera;
mod renderer;
mod shaders;
mod world;
mod worms;

use camera::Camera;
use glam::{Vec2, Vec3};
use renderer::{FrameParams, Gpu, Offscreen, Renderer};
use sdf::{Grid, Op, Shape};
use sim::{dig_target, Input, World, DIG_RADIUS, DT, EYE_HEIGHT};
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
  W A S D           walk                     Space  jump        mouse  look and aim
  left mouse        dig (hold)               right mouse  shoot the rope / let go
  E / Q             reel the rope in (pulls you, so you can launch yourself) / out
  Space on the rope let go with an upward kick; steer while swinging to pump it up
  F                 fly camera with the M1 brush: W A S D Space Ctrl Shift fly,
                    left mouse carve, right mouse build, mouse wheel brush size
  1 2 3             fixed render resolution 100% / 75% / 50%
  T                 automatic resolution (default): holds ~60 fps by scaling it
  G shadows   H ambient occlusion   O ink outlines   V vsync   R reset the world";

struct Config {
    shot: Option<String>,
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
    brush: bool,
    shadows: bool,
    ao: bool,
    outlines: bool,
}

fn parse_args() -> Config {
    let mut c = Config {
        shot: None,
        bench: false,
        size: (1280, 720),
        scale: 1.0,
        auto_res: true,
        cam: None,
        edits: false,
        worm: None,
        rope: false,
        brush: true,
        shadows: true,
        ao: true,
        outlines: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| panic!("{name} needs a value"));
        match a.as_str() {
            "--shot" => c.shot = Some(value("--shot")),
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
            "--no-brush" => c.brush = false,
            "--no-shadows" => c.shadows = false,
            "--no-ao" => c.ao = false,
            "--no-outlines" => c.outlines = false,
            other => panic!("unknown argument {other}\n{HELP}"),
        }
    }
    c
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

/// The point the crosshair is on, if the camera is inside the world.
fn aim_point(grid: &Grid, cam: &Camera) -> Option<Vec3> {
    if !in_world(grid, cam.pos) {
        return None;
    }
    let dir = cam.forward();
    grid.raycast(cam.pos, dir, 100.0).map(|t| cam.pos + dir * t)
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
    if cfg.bench {
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
fn shot_world(cfg: &Config) -> World {
    let spawns: Vec<(f32, f32)> = cfg.worm.iter().map(|w| (w[0], w[1])).collect();
    let mut world = World::new(world::build(), &spawns);
    if let Some([_, _, yaw]) = cfg.worm {
        let look = Input { yaw: yaw.to_radians(), ..Input::default() };
        for _ in 0..60 {
            world.step(&[look]);
        }
        if cfg.rope {
            // Shoot the rope up and forward, then let it swing for a third of a second.
            let shoot = Input { pitch: 0.7, rope: true, ..look };
            world.step(&[shoot]);
            for _ in 0..20 {
                world.step(&[Input { rope: false, ..shoot }]);
            }
        }
    }
    for (i, w) in world.worms.iter().enumerate() {
        println!("worm {i} stands at {:.1}, {:.1}, {:.1}{}", w.pos.x, w.pos.y, w.pos.z, if w.rope.is_some() { " on its rope" } else { "" });
    }
    world
}

fn run_shot(cfg: &Config, path: &str) {
    let gpu = headless_gpu();
    let mut world = shot_world(cfg);
    let mut renderer = Renderer::new(&gpu, Offscreen::FORMAT, cfg.size, cfg.scale, &mut world.grid);
    if cfg.edits {
        demo_edits(&mut world.grid);
        println!("synced {} chunks", renderer.sync(&mut world.grid));
    }
    let cam = camera_from(cfg);
    let target = Offscreen::new(&gpu.device, cfg.size);
    let positions: Vec<Vec3> = world.worms.iter().map(|w| w.pos).collect();
    let capsules = worms::capsules(&world.worms, &positions);
    let params = FrameParams {
        camera: &cam,
        capsules: &capsules,
        brush: if cfg.brush { aim_point(&world.grid, &cam).map(|p| (p, 3.0)) } else { None },
        time: 0.0,
        shadows: cfg.shadows,
        ao: cfg.ao,
        outlines: cfg.outlines,
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
            let params = FrameParams { camera: cam, capsules: &[], brush: None, time: 0.0, shadows, ao, outlines };
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
    let capsules = worms::capsules(&worms, &positions);
    let with = FrameParams { camera: &views[0].1, capsules: &capsules, brush: None, time: 0.0, shadows: true, ao: true, outlines: true };
    let without = FrameParams { capsules: &[], ..with };
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
    let params = FrameParams { camera: cam, capsules: &[], brush: None, time: 0.0, shadows: true, ao: true, outlines: true };
    let (mut edit, mut sync, mut chunks) = (0.0, 0.0, 0usize);
    let n = 100;
    for i in 0..n {
        let x = 20.0 + (i % 10) as f32 * 9.0;
        let z = 20.0 + (i / 10) as f32 * 9.0;
        let Some(t) = grid.raycast(Vec3::new(x, 60.0, z), -Vec3::Y, 80.0) else { continue };
        let t0 = Instant::now();
        carve(&mut grid, Vec3::new(x, 60.0 - t, z), 3.0);
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
    let t = Instant::now();
    for _ in 0..50 {
        std::hint::black_box(renderer::coarse_map(&grid));
    }
    println!("empty-space map rebuild: {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 50.0);
    println!(
        "r=3 m crater x{n}: CPU edit {:.2} ms, upload {:.2} ms ({:.1} chunks) per edit",
        edit / n as f64,
        sync / n as f64,
        chunks as f64 / n as f64
    );
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
    seq: u32,
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
                        .with_title("liero3d M2")
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
        let mut world = World::new(world::build(), &world::SPAWNS);
        let renderer = Renderer::new(&gpu, format, (surface_cfg.width, surface_cfg.height), cfg.scale, &mut world.grid);
        let prev_pos = world.worms.iter().map(|w| w.pos).collect();
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
            seq: 0,
            // An explicit --cam means "look at this spot", which only the free camera can.
            fly: cfg.cam.is_some(),
            camera: camera_from(cfg),
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
            KeyCode::Digit1 => self.set_fixed_scale(1.0),
            KeyCode::Digit2 => self.set_fixed_scale(0.75),
            KeyCode::Digit3 => self.set_fixed_scale(0.5),
            KeyCode::KeyT => {
                self.auto_res = !self.auto_res;
                self.gpu_ms = 0.0;
            }
            KeyCode::KeyG => self.shadows = !self.shadows,
            KeyCode::KeyH => self.ao = !self.ao,
            KeyCode::KeyO => self.outlines = !self.outlines,
            KeyCode::KeyV => {
                self.vsync = !self.vsync;
                self.surface_cfg.present_mode =
                    if self.vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
                self.surface.configure(&self.gpu.device, &self.surface_cfg);
            }
            KeyCode::KeyF => self.fly = !self.fly,
            KeyCode::KeyR => {
                self.world = World::new(world::build(), &world::SPAWNS);
                self.prev_pos = self.world.worms.iter().map(|w| w.pos).collect();
                self.renderer = Renderer::new(
                    &self.gpu,
                    self.surface_cfg.format,
                    (self.surface_cfg.width, self.surface_cfg.height),
                    self.renderer.scale(),
                    &mut self.world.grid,
                );
            }
            _ => {}
        }
    }

    /// What the keyboard and mouse ask worm 0 to do this tick.
    fn worm_input(&self) -> Input {
        let worm = &self.world.worms[0];
        if self.fly {
            // The free camera has the keys: the worm just stands and looks as before.
            return Input { yaw: worm.yaw, pitch: worm.pitch, ..Input::default() };
        }
        let held = |k| self.keys.contains(&k);
        let axis = |plus, minus| (if held(plus) { 1.0 } else { 0.0 }) - (if held(minus) { 1.0 } else { 0.0 });
        Input {
            seq: 0, // set per tick in `update`
            move_dir: Vec2::new(axis(KeyCode::KeyD, KeyCode::KeyA), axis(KeyCode::KeyW, KeyCode::KeyS)),
            yaw: self.camera.yaw,
            pitch: self.camera.pitch,
            jump: held(KeyCode::Space),
            dig: self.grabbed && self.lmb,
            rope: self.grabbed && self.rmb,
            reel: axis(KeyCode::KeyE, KeyCode::KeyQ),
        }
    }

    fn update(&mut self, dt: f32) {
        // Pattern: Fixed Timestep. The sim always advances in exact 1/60 s ticks,
        // however fast frames come: it adds the real time to an accumulator and
        // runs as many ticks as fit. (The frame time is capped at 0.1 s so a
        // long stall cannot trigger a burst of ticks.) What is left in `acc` is
        // how far we are into the next tick; `frame` uses it to draw smoothly.
        self.acc += dt;
        let mut input = self.worm_input();
        let t = Instant::now();
        let mut ticks = 0;
        while self.acc >= DT {
            self.prev_pos = self.world.worms.iter().map(|w| w.pos).collect();
            input.seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            self.world.step(&[input]);
            self.acc -= DT;
            ticks += 1;
        }
        if ticks > 0 {
            self.sim_ms = t.elapsed().as_secs_f64() * 1000.0;
        }
        // Nothing listens to the sim's events yet (sound and particles will, at
        // M6); drain them so they do not pile up. The renderer needs no event
        // for terrain edits: it asks the grid which chunks changed.
        self.world.drain_events();

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

        // Draw each worm between where it was a tick ago and where it is now.
        // Without this, on a display that is not exactly 60 Hz some frames would
        // show two ticks of movement and some none, and the worm would judder.
        let alpha = (self.acc / DT).clamp(0.0, 1.0);
        let draw_pos: Vec<Vec3> =
            self.world.worms.iter().zip(&self.prev_pos).map(|(w, &p)| p.lerp(w.pos, alpha)).collect();
        // Speed widens the view: the world streams past faster at the edges,
        // which is a cheap way to make fast movement FEEL fast.
        const CALM_SPEED: f32 = 8.0; // walking speed: no widening up to here
        let speed = if self.fly { 0.0 } else { self.world.worms[0].vel.length() };
        let widen = ((speed - CALM_SPEED) / 16.0).clamp(0.0, 1.0);
        let target_fov = (70.0 + 14.0 * widen).to_radians();
        self.camera.fov_y += (target_fov - self.camera.fov_y) * (dt * 5.0).min(1.0);
        if !self.fly {
            self.camera.follow(draw_pos[0] + Vec3::Y * (EYE_HEIGHT + 0.2), 5.0, &self.world.grid);
        }
        let capsules = worms::capsules(&self.world.worms, &draw_pos);
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
            camera: &self.camera,
            capsules: &capsules,
            brush,
            time: (now - self.started).as_secs_f32(),
            shadows: self.shadows,
            ao: self.ao,
            outlines: self.outlines,
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
            self.window.set_title(&format!(
                "liero3d M2 {} | {:.0} fps ({:.1} ms) | gpu {:.1} ms (march {:.1} + post {:.1}) | sim {:.2} ms, edit {:.2} ms, upload {:.2} ms ({} chunks) | brush {:.1} m | res {:.0}%{} | shadows {} ao {} ink {} vsync {}",
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
                v.brush_radius = (v.brush_radius * 1.12f32.powf(lines)).clamp(0.5, 8.0);
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
