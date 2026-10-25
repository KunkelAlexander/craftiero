// A studio for the worm's body, separate from the game.
//
// The point is to judge the sculpt before it is wired into the sim: one image
// with eight yaws, shaded and in flat silhouette, at gameplay size and at the
// size where a worm is a dozen pixels. No terrain, no gun code, no animation.
// If the black shapes do not read here, no amount of shading will save them.
//
//   cargo run -p viewer --release -- --worm-preview --shot worm.png \
//       --size 1920x750
//
// The body is one signed distance field: a tapered spine, a head with a snout,
// a cut mouth, limbs and feet, all joined with smooth minimums. That is the
// whole argument for leaving the capsule stack behind: a single surface means
// one silhouette, one outline, and joins that actually flow.

use crate::renderer::{Gpu, Offscreen};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    size: [f32; 2],
    _pad: [f32; 2],
}

pub fn run(gpu: &Gpu, size: (u32, u32), path: &str) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("worm preview uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(
        &uniforms,
        0,
        bytemuck::bytes_of(&Uniforms { size: [size.0 as f32, size.1 as f32], _pad: [0.0; 2] }),
    );

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("worm preview layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("worm preview bind"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("worm preview"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("worm preview"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("worm preview"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: Offscreen::FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let target = Offscreen::new(device, size);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("worm preview"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    let pixels = target.read(gpu);

    let file = std::io::BufWriter::new(std::fs::File::create(path).expect("cannot create the PNG"));
    let mut enc = png::Encoder::new(file, size.0, size.1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("png header").write_image_data(&pixels).expect("png data");
    println!("wrote {path} ({}x{})", size.0, size.1);
}

// One sheet: 8 columns x 3 rows. Top row shaded; middle row silhouettes at the
// same size; bottom row silhouettes at a distance where the worm is ~60 px, the
// size it is on screen while you play.
const SHADER: &str = r#"
struct U {
    size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: U;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// --- distances -------------------------------------------------------------

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

fn smax(a: f32, b: f32, k: f32) -> f32 {
    return -smin(-a, -b, k);
}

fn sd_sphere(p: vec3<f32>, c: vec3<f32>, r: f32) -> f32 {
    return length(p - c) - r;
}

// A good approximation, not an exact distance: step under it in `trace`.
fn sd_ellipsoid(p: vec3<f32>, c: vec3<f32>, r: vec3<f32>) -> f32 {
    let q = (p - c) / r;
    let k0 = length(q);
    let k1 = length(q / r);
    return k0 * (k0 - 1.0) / max(k1, 1.0e-6);
}

// Inigo Quilez's exact round cone: the workhorse for every tapered limb, the
// spine and the snout. Two radii and a segment are enough to describe a worm.
fn sd_round_cone(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, r1: f32, r2: f32) -> f32 {
    let ba = b - a;
    let l2 = dot(ba, ba);
    let rr = r1 - r2;
    let a2 = l2 - rr * rr;
    let il2 = 1.0 / l2;

    let pa = p - a;
    let y = dot(pa, ba);
    let z = y - l2;
    let x2 = dot(pa * l2 - ba * y, pa * l2 - ba * y);
    let y2 = y * y * l2;
    let z2 = z * z * l2;

    let k = sign(rr) * rr * rr * x2;
    if (sign(z) * a2 * z2 > k) { return sqrt(x2 + z2) * il2 - r2; }
    if (sign(y) * a2 * y2 < k) { return sqrt(x2 + y2) * il2 - r1; }
    return (sqrt(x2 * a2 * il2) + y * rr) * il2 - r1;
}

fn sd_box(p: vec3<f32>, c: vec3<f32>, h: vec3<f32>) -> f32 {
    let q = abs(p - c) - h;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

fn sd_capsule(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, r: f32) -> f32 {
    let ba = b - a;
    let h = clamp(dot(p - a, ba) / dot(ba, ba), 0.0, 1.0);
    return length(p - a - ba * h) - r;
}

// --- the worm --------------------------------------------------------------

const EYE_L = vec3<f32>(-0.175, 1.300, 0.520);
const EYE_R = vec3<f32>( 0.175, 1.300, 0.520);
const EYE_RADIUS: f32 = 0.062;
const NOSTRIL_L = vec3<f32>(-0.033, 1.180, 0.735);
const NOSTRIL_R = vec3<f32>( 0.033, 1.180, 0.735);
const GUN_C = vec3<f32>(0.10, 0.96, 0.78);

fn sd_mouth(p: vec3<f32>) -> f32 {
    // A wide grin, cut into the snout. The lower lip below it hides the bottom
    // of the ellipse; the tongue fills the middle.
    return sd_ellipsoid(p, vec3<f32>(0.0, 1.100, 0.710), vec3<f32>(0.110, 0.040, 0.055));
}

fn sd_lower_lip(p: vec3<f32>) -> f32 {
    return sd_ellipsoid(p, vec3<f32>(0.0, 1.070, 0.710), vec3<f32>(0.088, 0.020, 0.038));
}

fn sd_tongue(p: vec3<f32>) -> f32 {
    return sd_ellipsoid(p, vec3<f32>(0.0, 1.082, 0.725), vec3<f32>(0.060, 0.018, 0.028));
}

fn sd_socket(p: vec3<f32>) -> f32 {
    // A shallow hole for each eye, so the bead sits in the face instead of on
    // it. The rim is what casts the dark line the brow sits over.
    var d = sd_ellipsoid(p, EYE_L, vec3<f32>(0.078, 0.078, 0.070));
    d = min(d, sd_ellipsoid(p, EYE_R, vec3<f32>(0.078, 0.078, 0.070)));
    return d;
}

fn sd_brow(p: vec3<f32>) -> f32 {
    // The grumpy brow from the upright pass: inner end high, outer end low.
    // A skin ridge, not a painted line; it sits on the eye's top rim.
    var d = sd_round_cone(p, vec3<f32>(-0.060, 1.365, 0.535), vec3<f32>(-0.250, 1.350, 0.485), 0.030, 0.020);
    d = min(d, sd_round_cone(p, vec3<f32>(0.060, 1.365, 0.535), vec3<f32>(0.250, 1.350, 0.485), 0.030, 0.020));
    return d;
}

fn sd_eyelid(p: vec3<f32>) -> f32 {
    // A thin skin fold over the top of the eye. It narrows the bead into a
    // glare without covering enough to make it sleepy.
    var d = sd_ellipsoid(p, vec3<f32>(-0.175, 1.347, 0.535), vec3<f32>(0.080, 0.013, 0.062));
    d = min(d, sd_ellipsoid(p, vec3<f32>( 0.175, 1.347, 0.535), vec3<f32>(0.080, 0.013, 0.062)));
    return d;
}

fn sd_gun(p: vec3<f32>) -> f32 {
    var d = sd_box(p, GUN_C, vec3<f32>(0.075, 0.095, 0.130));
    d = min(d, sd_round_cone(p, vec3<f32>(0.10, 0.96, 0.90), vec3<f32>(0.10, 0.96, 1.280), 0.045, 0.034));
    d = min(d, sd_sphere(p, vec3<f32>(0.10, 0.96, 1.305), 0.055));
    d = min(d, sd_box(p, vec3<f32>(0.10, 0.835, 0.690), vec3<f32>(0.034, 0.095, 0.050)));
    d = min(d, sd_box(p, vec3<f32>(0.10, 0.865, 0.840), vec3<f32>(0.030, 0.055, 0.040)));
    d = min(d, sd_box(p, vec3<f32>(0.10, 1.058, 0.765), vec3<f32>(0.030, 0.012, 0.090)));
    return d;
}

// The gun's hot strip, used only for colour.
fn sd_cell(p: vec3<f32>) -> f32 {
    return sd_box(p, vec3<f32>(0.10, 1.074, 0.765), vec3<f32>(0.022, 0.005, 0.065));
}

fn sd_arms(p: vec3<f32>) -> f32 {
    // Both hands sit on the weapon: the right on the grip, the left crossing to
    // the foregrip. This is what stops the gun reading as a floating prop.
    var d = sd_round_cone(p, vec3<f32>(0.230, 0.980, 0.180), vec3<f32>(0.345, 0.910, 0.370), 0.098, 0.076);
    d = smin(d, sd_round_cone(p, vec3<f32>(0.345, 0.910, 0.370), vec3<f32>(0.115, 0.875, 0.660), 0.076, 0.058), 0.018);
    d = smin(d, sd_sphere(p, vec3<f32>(0.108, 0.868, 0.682), 0.086), 0.018);

    var l = sd_round_cone(p, vec3<f32>(-0.230, 0.980, 0.180), vec3<f32>(-0.335, 0.915, 0.375), 0.098, 0.076);
    l = smin(l, sd_round_cone(p, vec3<f32>(-0.335, 0.915, 0.375), vec3<f32>(0.085, 0.882, 0.838), 0.076, 0.055), 0.018);
    l = smin(l, sd_sphere(p, vec3<f32>(0.100, 0.870, 0.850), 0.078), 0.018);
    return min(d, l);
}

fn map(p: vec3<f32>) -> f32 {
    // Spine: the hips sit on the ground, the chest leans forward and the head
    // is well in front of the tail. A C, not a column: this is the worm's shape.
    var d = sd_round_cone(p, vec3<f32>(0.0, 0.280, -0.080), vec3<f32>(0.0, 0.520, 0.030), 0.220, 0.240);
    d = smin(d, sd_round_cone(p, vec3<f32>(0.0, 0.520, 0.030), vec3<f32>(0.0, 0.800, 0.140), 0.240, 0.230), 0.055);
    d = smin(d, sd_round_cone(p, vec3<f32>(0.0, 0.800, 0.140), vec3<f32>(0.0, 1.020, 0.260), 0.230, 0.180), 0.050);
    d = smin(d, sd_round_cone(p, vec3<f32>(0.0, 1.020, 0.260), vec3<f32>(0.0, 1.120, 0.300), 0.180, 0.180), 0.035);

    // Head: a skull with wide cheeks, tapering into a snout. The eyes sit on
    // the sides of it, like a lizard's, instead of on a human face.
    d = smin(d, sd_ellipsoid(p, vec3<f32>(0.0, 1.270, 0.350), vec3<f32>(0.250, 0.240, 0.240)), 0.038);
    d = smin(d, sd_sphere(p, vec3<f32>(-0.130, 1.210, 0.400), 0.130), 0.036);
    d = smin(d, sd_sphere(p, vec3<f32>( 0.130, 1.210, 0.400), 0.130), 0.036);

    // The snout is a taper, not a ball. It is the shape the old model could
    // never make: a forward wedge blended into the head.
    d = smin(d, sd_round_cone(p, vec3<f32>(0.0, 1.200, 0.410), vec3<f32>(0.0, 1.140, 0.640), 0.170, 0.090), 0.032);
    d = smin(d, sd_ellipsoid(p, vec3<f32>(0.0, 1.130, 0.680), vec3<f32>(0.090, 0.080, 0.075)), 0.022);
    d = smin(d, sd_ellipsoid(p, vec3<f32>(0.0, 1.020, 0.520), vec3<f32>(0.115, 0.060, 0.090)), 0.026);
    d = smin(d, sd_lower_lip(p), 0.015);

    // Sockets, grumpy brows and a thin lid. These are the expression; the
    // body underneath stays the shape of preview 8.
    d = smax(d, -sd_socket(p), 0.010);
    d = smin(d, sd_brow(p), 0.020);
    d = smin(d, sd_eyelid(p), 0.012);

    // A real cut grin. The cavity is what makes a face read; a raised dark bar
    // (what the capsule worm had) never does.
    d = smax(d, -sd_mouth(p), 0.011);
    d = min(d, sd_tongue(p));

    // A tapering tail rests on the ground and curls up at the tip: the worm's
    // base, and the asymmetry that makes the back view readable.
    var t = sd_round_cone(p, vec3<f32>(0.0, 0.280, -0.100), vec3<f32>(0.050, 0.150, -0.320), 0.160, 0.110);
    t = smin(t, sd_round_cone(p, vec3<f32>(0.050, 0.150, -0.320), vec3<f32>(0.160, 0.090, -0.500), 0.110, 0.070), 0.028);
    t = smin(t, sd_round_cone(p, vec3<f32>(0.160, 0.090, -0.500), vec3<f32>(0.240, 0.080, -0.620), 0.070, 0.045), 0.022);
    t = smin(t, sd_round_cone(p, vec3<f32>(0.240, 0.080, -0.620), vec3<f32>(0.180, 0.120, -0.700), 0.045, 0.025), 0.018);
    d = smin(d, t, 0.035);

    d = smin(d, sd_arms(p), 0.030);

    // Eyes are a hard union: they must stay spherical, with a crisp socket.
    d = min(d, sd_sphere(p, EYE_L, EYE_RADIUS));
    d = min(d, sd_sphere(p, EYE_R, EYE_RADIUS));

    // The held gun is part of the silhouette, hard-edged against the soft body.
    d = min(d, sd_gun(p));
    return d;
}

fn normal_at(p: vec3<f32>) -> vec3<f32> {
    let e = 0.0015;
    return normalize(vec3<f32>(
        map(p + vec3<f32>(e, 0.0, 0.0)) - map(p - vec3<f32>(e, 0.0, 0.0)),
        map(p + vec3<f32>(0.0, e, 0.0)) - map(p - vec3<f32>(0.0, e, 0.0)),
        map(p + vec3<f32>(0.0, 0.0, e)) - map(p - vec3<f32>(0.0, 0.0, e)),
    ));
}

// Ray/sphere clip around the whole character: most pixels never enter.
const BOUND_C = vec3<f32>(0.0, 0.90, 0.18);
const BOUND_R: f32 = 1.30;

fn bound_hit(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    let oc = ro - BOUND_C;
    let b = dot(oc, rd);
    let c = dot(oc, oc) - BOUND_R * BOUND_R;
    let h = b * b - c;
    if (h < 0.0) { return vec2<f32>(1.0, -1.0); }
    let s = sqrt(h);
    return vec2<f32>(-b - s, -b + s);
}

fn trace_worm(ro: vec3<f32>, rd: vec3<f32>, tmax: f32) -> f32 {
    let b = bound_hit(ro, rd);
    if (b.y < max(b.x, 0.0)) { return -1.0; }
    var t = max(b.x, 0.0);
    let end = min(b.y, tmax);
    for (var i = 0; i < 120; i++) {
        let d = map(ro + rd * t);
        if (d < 0.0007) { return t; }
        t += max(d * 0.72, 0.0008);
        if (t > end) { break; }
    }
    return -1.0;
}

// A shadow ray only needs a yes/no: march the body, no material.
fn blocked(ro: vec3<f32>, rd: vec3<f32>, tmax: f32) -> bool {
    let b = bound_hit(ro, rd);
    if (b.y < max(b.x, 0.0)) { return false; }
    var t = max(b.x, 0.0);
    let end = min(b.y, tmax);
    for (var i = 0; i < 48; i++) {
        let d = map(ro + rd * t);
        if (d < 0.002) { return true; }
        t += max(d, 0.004);
        if (t > end) { break; }
    }
    return false;
}

fn ao_at(p: vec3<f32>, n: vec3<f32>) -> f32 {
    var occ = 0.0;
    var w = 1.0;
    for (var i = 1; i <= 5; i++) {
        let h = 0.020 + 0.055 * f32(i);
        let d = map(p + n * h);
        occ += (h - d) * w;
        w *= 0.72;
    }
    return clamp(1.0 - 0.62 * occ, 0.35, 1.0);
}

fn srgb(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

const SKIN = vec3<f32>(0.95, 0.44, 0.34);
const SKIN_DARK = vec3<f32>(0.80, 0.32, 0.26);
const BELLY = vec3<f32>(0.99, 0.80, 0.64);
const PUPIL = vec3<f32>(0.045, 0.055, 0.085);
const MOUTH_COL = vec3<f32>(0.30, 0.075, 0.095);
const TONGUE_COL = vec3<f32>(0.93, 0.42, 0.45);
const NOSE_COL = vec3<f32>(0.70, 0.27, 0.25);
const GUNMETAL = vec3<f32>(0.28, 0.30, 0.38);
const GUN_DARK = vec3<f32>(0.16, 0.17, 0.23);
const CYAN = vec3<f32>(0.35, 0.92, 1.0);

fn colour(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    // Eyes: small black beads on the sides of the snout, with one catchlight.
    // That is the Liero read; big white human eyes made the model a baby.
    let dl = length(p - EYE_L);
    let dr = length(p - EYE_R);
    if (min(abs(dl - EYE_RADIUS), abs(dr - EYE_RADIUS)) < 0.003) {
        let c = select(EYE_R, EYE_L, dl < dr);
        let dir = normalize(p - c);
        let hl = normalize(vec3<f32>(-0.35, 0.55, 0.75));
        if (dot(dir, hl) > 0.940) { return vec3<f32>(1.0); }
        return PUPIL;
    }

    // The gun, including its cyan strip.
    if (sd_gun(p) < 0.006) {
        if (sd_cell(p) < 0.004) { return CYAN; }
        if (p.z < 0.70 && p.y < 0.86) { return GUN_DARK; }
        return GUNMETAL;
    }

    // The grin's cut is dark, with a small tongue in it.
    if (sd_mouth(p) < 0.018) {
        if (sd_tongue(p) < 0.006) { return TONGUE_COL; }
        return MOUTH_COL;
    }

    var c = SKIN;
    // Cream belly, painted by normal rather than built from another capsule.
    let belly_x = smoothstep(0.30, 0.12, abs(p.x));
    let belly_y = smoothstep(1.04, 0.94, p.y) * smoothstep(0.28, 0.62, n.z);
    let belly = belly_x * belly_y;
    c = mix(c, BELLY, belly);

    // Faint segment bands on the belly only: a worm cue with no extra geometry.
    let band = abs(fract((p.y - 0.25) * 5.5) - 0.5);
    let crease = smoothstep(0.40, 0.47, band) * belly;
    c = mix(c, BELLY * 0.88, crease * 0.55);

    // Four or five shallow rings down the body and tail, perpendicular to the
    // spine. Without them the long body reads as a slug.
    let seg = abs(fract((p.y * 0.9 - p.z * 0.7) * 5.0) - 0.5);
    let ring = smoothstep(0.44, 0.49, seg) * smoothstep(1.02, 0.94, p.y);
    c *= 1.0 - 0.10 * ring * (1.0 - belly);

    // A lighter underside on the snout, and two dark nostrils at the tip.
    let muzzle = smoothstep(1.25, 1.16, p.y) * smoothstep(0.48, 0.58, p.z) * smoothstep(0.94, 1.01, p.y);
    c = mix(c, BELLY, muzzle * 0.60);
    if ((length(p - NOSTRIL_L) < 0.013 || length(p - NOSTRIL_R) < 0.013) && p.z > 0.690) {
        c = NOSE_COL;
    }
    if (sd_brow(p) < 0.010) { c *= 0.86; }
    if (sd_eyelid(p) < 0.008) { c *= 0.94; }

    // Hands read as separate masses through value, not seams. The head keeps
    // one continuous surface.
    if (sd_arms(p) < 0.008 && p.z > 0.55) { c = mix(c, SKIN_DARK, 0.45); }
    if (p.y < 0.10) { c = mix(c, SKIN_DARK, 0.25); }
    return c;
}

fn toon(x: f32) -> f32 {
    return smoothstep(0.0, 0.04, x) * 0.55 + smoothstep(0.42, 0.46, x) * 0.45;
}

fn shade_worm(p: vec3<f32>, n: vec3<f32>, ro: vec3<f32>) -> vec3<f32> {
    let base = srgb(colour(p, n));
    let L = normalize(vec3<f32>(0.55, 0.78, 0.32));
    let lit = max(dot(n, L), 0.0);
    let shadow = select(1.0, 0.0, blocked(p + n * 0.02, L, 3.0));
    let light = toon(lit) * shadow;
    let tint = mix(vec3<f32>(0.56, 0.58, 0.74), vec3<f32>(1.15, 1.06, 0.95), light);
    var c = base * tint * mix(0.55, 1.0, ao_at(p, n));

    // A cool rim and a small specular, the same language as the game.
    let v = normalize(ro - p);
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0);
    c += vec3<f32>(0.55, 0.72, 1.0) * rim * 0.18;
    let h = normalize(L + v);
    let spec = pow(max(dot(n, h), 0.0), 32.0) * light;
    c += vec3<f32>(1.0, 0.95, 0.85) * spec * 0.30;
    return c;
}

fn backdrop(uv: vec2<f32>) -> vec3<f32> {
    let top = vec3<f32>(0.91, 0.93, 0.96);
    let bot = vec3<f32>(0.78, 0.81, 0.87);
    var c = mix(bot, top, smoothstep(0.0, 1.0, uv.y));
    let d = length(uv - vec2<f32>(0.5, 0.46));
    c *= 1.0 - 0.08 * smoothstep(0.25, 0.85, d);
    return srgb(c);
}

fn shade_ground(p: vec3<f32>, ro: vec3<f32>) -> vec3<f32> {
    let base = srgb(vec3<f32>(0.76, 0.77, 0.79));
    let d = length(p.xz);
    var c = base * (1.0 - 0.20 * exp(-d * d * 2.2));
    let L = normalize(vec3<f32>(0.55, 0.78, 0.32));
    if (blocked(p + vec3<f32>(0.0, 0.012, 0.0), L, 3.5)) { c *= 0.58; }
    // Fade into the backdrop so the studio has no hard horizon.
    let bg = backdrop(vec2<f32>(0.5, 0.2));
    return mix(c, bg, smoothstep(2.5, 7.5, length(p - ro)));
}

fn look_at(ro: vec3<f32>, goal: vec3<f32>) -> mat3x3<f32> {
    let f = normalize(goal - ro);
    let r = normalize(cross(f, vec3<f32>(0.0, 1.0, 0.0)));
    let u = cross(r, f);
    return mat3x3<f32>(r, u, f);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = frag.xy / u.size.xy;
    let grid = vec2<f32>(8.0, 3.0);
    let cell = min(vec2<u32>(uv * grid), vec2<u32>(7u, 2u));
    let luv = fract(uv * grid);
    let cell_px = u.size / grid;

    // One-pixel rules between cells make the sheet read as a contact sheet.
    let edge = min(min(luv.x, 1.0 - luv.x) * cell_px.x, min(luv.y, 1.0 - luv.y) * cell_px.y);
    if (edge < 1.0) { return vec4<f32>(0.82, 0.82, 0.82, 1.0); }

    let row = cell.y;
    let yaw = f32(cell.x) * 0.7853982;
    let dist = select(4.0, 12.8, row == 2u);
    let focus = vec3<f32>(0.0, 0.90, 0.10);
    let pitch = 0.17;
    let dir = vec3<f32>(sin(yaw) * cos(pitch), sin(pitch), cos(yaw) * cos(pitch));
    let ro = focus + dir * dist;
    let basis = look_at(ro, focus);

    let aspect = cell_px.x / cell_px.y;
    let fov = 0.5585054; // 32 degrees
    let ndc = vec2<f32>((luv.x * 2.0 - 1.0) * aspect, (1.0 - luv.y * 2.0));
    let rd = normalize(basis * vec3<f32>(ndc * tan(fov * 0.5), 1.0));

    let tw = trace_worm(ro, rd, 50.0);
    let bg = backdrop(luv);

    if (row >= 1u) {
        if (tw > 0.0) { return vec4<f32>(0.035, 0.035, 0.045, 1.0); }
        return vec4<f32>(bg, 1.0);
    }

    var col = bg;
    // Floor first: the worm is small, so the plane is the cheap default.
    if (rd.y < -0.0001) {
        let tp = -ro.y / rd.y;
        if (tp > 0.0) {
            let pg = ro + rd * tp;
            if (tw < 0.0 || tp < tw) { col = shade_ground(pg, ro); }
        }
    }
    if (tw > 0.0) {
        let p = ro + rd * tw;
        col = shade_worm(p, normal_at(p), ro);
    }
    return vec4<f32>(col, 1.0);
}
"#;
