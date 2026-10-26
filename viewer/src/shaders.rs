// Shaders as source strings (no asset files). Two passes:
//
//   1. MARCH: sphere-trace the terrain SDF per pixel, shade it with a toon
//      ramp, and write colour plus (normal, distance) into a G-buffer.
//   2. POST:  find ink outlines in the G-buffer and composite to the screen.
//
// The world constants are pasted in from the `sdf` crate, so the shader can
// never disagree with the CPU about voxel size or world extent.

use crate::renderer::{MAX_PRIMS, TILES_X, TILES_Y};
use glam::IVec3;

fn f(x: f32) -> String {
    format!("{x:?}") // {:?} keeps the ".0" that WGSL needs on float literals
}

fn vec3(v: glam::Vec3) -> String {
    format!("vec3<f32>({}, {}, {})", f(v.x), f(v.y), f(v.z))
}

/// The uniform block with the capsule/tile counts pasted in from the renderer,
/// so the WGSL can never disagree with the CPU about how many there are.
fn uniforms() -> String {
    let groups = TILES_X * TILES_Y / 4;
    COMMON_UNIFORMS
        .replace("$WORMS", &crate::renderer::MAX_WORMS.to_string())
        .replace("$CAPS", &MAX_PRIMS.to_string())
        .replace("$TILES", &(crate::renderer::TILE_WORDS * groups).to_string())
        .replace("$WORDS", &crate::renderer::TILE_WORDS.to_string())
        .replace("$GROUPS", &groups.to_string())
}

/// The shared worm sculpt with its rest-pose hands pasted in. The hand numbers
/// live in `worms.rs`, where the animation and the gun use them, so the sheet
/// and the game cannot disagree.
pub fn sculpt() -> String {
    WORM_SCULPT
        .replace("$HAND_R", &vec3(crate::worms::SDF_HAND_R))
        .replace("$HAND_L", &vec3(crate::worms::SDF_HAND_L))
}

pub fn march(world_chunks: IVec3) -> String {
    let dims = (world_chunks * sdf::CHUNK).as_vec3();
    let metres = dims * sdf::VOX;
    format!("{}{}{WORM_TRACE}{MARCH}", uniforms(), sculpt())
        .replace("$VOX", &f(sdf::VOX))
        .replace("$BAND", &f(sdf::BAND))
        .replace("$CHUNK_M", &f(sdf::CHUNK as f32 * sdf::VOX))
        .replace("$DIMS", &format!("{}, {}, {}", f(dims.x), f(dims.y), f(dims.z)))
        .replace("$WORLD_M", &format!("{}, {}, {}", f(metres.x), f(metres.y), f(metres.z)))
        .replace("$CHUNKS", &format!("{}, {}, {}", world_chunks.x, world_chunks.y, world_chunks.z))
        .replace("$WORMS", &crate::renderer::MAX_WORMS.to_string())
        .replace("$WORDS", &crate::renderer::TILE_WORDS.to_string())
        .replace("$GROUPS", &(TILES_X * TILES_Y / 4).to_string())
}

pub fn post() -> String {
    format!("{}{POST}", uniforms())
}

pub fn sprites(world_chunks: IVec3) -> String {
    let dims = (world_chunks * sdf::CHUNK).as_vec3();
    format!("{}{SPRITES}", uniforms())
        .replace("$VOX", &f(sdf::VOX))
        .replace("$BAND", &f(sdf::BAND))
        .replace("$DIMS", &format!("{}, {}, {}", f(dims.x), f(dims.y), f(dims.z)))
}

// One uniform block shared by both passes. Everything is vec4 so the Rust side
// needs no padding thought (WGSL aligns vec3 to 16 bytes, which bites).
const COMMON_UNIFORMS: &str = r#"
// One analytic primitive, intersected in closed form (not sphere-traced). The
// fields mean different things per kind, read from `col.w`: 0 capsule, 1 flat
// cylinder, 2 cuboid. (`array<Prim, $CAPS>` must match MAX_PRIMS in renderer.rs:
// gameplay slots followed by the cosmetic blood slots.)
struct Prim {
    a: vec4<f32>,           // capsule/cylinder: start.xyz + radius; cuboid: centre.xyz
    b: vec4<f32>,           // capsule/cylinder: end.xyz; cuboid: half extents. w = bound radius
    c: vec4<f32>,           // cuboid: forward axis.xyz; capsule/cylinder: unused
    col: vec4<f32>,         // rgb (sRGB) + w: kind 0/1/2
}

// A dynamic point light: blasts, muzzle flashes, plasma. Must match `Light` in
// renderer.rs.
struct Light {
    a: vec4<f32>,           // xyz: position, w: radius
    b: vec4<f32>,           // rgb: colour (linear), w: intensity
}

// A sphere of soot left by a blast. Must match `Soot` in renderer.rs.
struct Soot {
    a: vec4<f32>,           // xyz: centre, w: radius
    b: vec4<f32>,           // x: strength 0..1
}



// One worm body, evaluated as a signed distance field. Must match `GpuWorm`
// in renderer.rs. The sculpt is duplicated from `worm_preview.rs`, where the
// studio sheet is rendered; the body there is the source of truth.
struct Worm {
    pivot: vec4<f32>,       // xyz: rig pivot, w: local y at the pivot
    right: vec4<f32>,       // xyz: unit right, w: 1 / scale along it
    up: vec4<f32>,          // xyz: unit up
    face: vec4<f32>,        // xyz: unit forward
    hand_r: vec4<f32>,      // xyz: right hand, in model units
    hand_l: vec4<f32>,      // xyz: left hand
    skin: vec4<f32>,        // rgb: sRGB skin, w: hit flash 0..1
    bound: vec4<f32>,       // xyz: centre, w: radius (0 = empty slot)
}

struct U {
    cam_pos: vec4<f32>,
    cam_right: vec4<f32>,   // right * tan(fov/2) * aspect
    cam_up: vec4<f32>,      // up * tan(fov/2)
    cam_fwd: vec4<f32>,
    sun: vec4<f32>,         // xyz: unit vector towards the sun
    brush: vec4<f32>,       // xyz: centre, w: radius (0 = no brush)
    flags: vec4<f32>,       // x: time, y: shadows, z: ao, w: outlines
    size: vec4<f32>,        // xy: G-buffer size, zw: screen size
    counts: vec4<f32>,      // x: capsules, y: shadow casters, z: soot, w: lights
    fx: vec4<f32>,          // x: screen flash 0..1, y: world desaturation 0..1 (zw free)
    lights: array<Light, 8>,
    soot: array<Soot, 8>,
    worms: array<Worm, $WORMS>,
    caps: array<Prim, $CAPS>,
    // 32 x 18 screen tiles (see TILES_X in renderer.rs), one bit per capsule:
    // which capsules can be seen in the tile. With up to $CAPS capsules each
    // tile needs $WORDS 32-bit words; word w starts at vec4 $GROUPS*w.
    tiles: array<vec4<u32>, $TILES>,
}
@group(0) @binding(0) var<uniform> u: U;

fn hash31(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += dot(q, q.zyx + 31.32);
    return fract((q.x + q.y) * q.z);
}

fn hash21(p: vec2<f32>) -> f32 {
    var q = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn vnoise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let fr = fract(p);
    let w = fr * fr * (3.0 - 2.0 * fr);
    return mix(
        mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), w.x),
        mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), w.x),
        w.y);
}

fn vnoise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let fr = fract(p);
    let w = fr * fr * (3.0 - 2.0 * fr);
    let a = mix(mix(hash31(i), hash31(i + vec3<f32>(1.0, 0.0, 0.0)), w.x),
                mix(hash31(i + vec3<f32>(0.0, 1.0, 0.0)), hash31(i + vec3<f32>(1.0, 1.0, 0.0)), w.x), w.y);
    let b = mix(mix(hash31(i + vec3<f32>(0.0, 0.0, 1.0)), hash31(i + vec3<f32>(1.0, 0.0, 1.0)), w.x),
                mix(hash31(i + vec3<f32>(0.0, 1.0, 1.0)), hash31(i + vec3<f32>(1.0, 1.0, 1.0)), w.x), w.y);
    return mix(a, b, w.z);
}

// Shared shading helpers. The march pass and the sprite passes must agree on
// these, or blood painted over the rock would have a different light on it than
// the rock itself.
// The palette is written the way you would pick colours in a paint program
// (sRGB). Shading happens in linear light, so convert on the way in: without
// this everything looks washed out.
fn srgb(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

// Three flat bands with a hair of smoothing so the edges do not alias.
fn toon(x: f32) -> f32 {
    return smoothstep(0.0, 0.04, x) * 0.55 + smoothstep(0.42, 0.46, x) * 0.45;
}

// Add the eight dynamic lights to a shaded surface. The falloff is squared and
// then banded with `toon`, so a fireball paints hard-edged pools of light on the
// rock in the same visual language as the sun instead of a smooth CG gradient.
fn lit_by_points(p: vec3<f32>, n: vec3<f32>, base: vec3<f32>, col: vec3<f32>) -> vec3<f32> {
    var out = col;
    for (var i = 0; i < i32(u.counts.w); i++) {
        let L = u.lights[i];
        if (L.b.w <= 0.001 || L.a.w <= 0.0) { continue; }
        let d = L.a.xyz - p;
        let dist2 = dot(d, d);
        let reach2 = L.a.w * L.a.w;
        if (dist2 > reach2) { continue; }
        let dist = sqrt(dist2);
        let near = max(1.0 - dist / L.a.w, 0.0);
        let fall = near * near;
        let ndl = max(dot(n, d / max(dist, 0.001)), 0.0);
        let band = 0.30 + 0.70 * toon(ndl);
        out += base * L.b.rgb * L.b.w * fall * band;
    }
    return out;
}

// The world is deliberately quieter than the actors. Most of the green is
// pulled out of rock, grass, sky and the arena wall, so the weapon palette,
// blood and health pips are the only strong colour on screen. Capsules are
// drawn after this and are never muted. Stains are blood, so they are muted
// with the ground they lie on.
fn muted(c: vec3<f32>, amount: f32) -> vec3<f32> {
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(c, vec3<f32>(l), amount);
}
"#;

/// The worm's body, shared by the game's march shader and the studio sheet in
/// `worm_preview.rs`. One signed distance field: a tapered spine, a head with a
/// snout and a cut grin, a curling tail, and two arms around the hands. Because
/// both shaders include this one copy, the sheet always shows the body that
/// ships.
///
/// Local frame: x right, y up from the feet, z forward. The game maps it through
/// the animation rig, so the same body leans, flips, squashes and takes a hit.
/// The hands are parameters: the sheet passes the rest pose, the game passes the
/// animated hands from `Worm`.
pub const WORM_SCULPT: &str = r#"
fn worm_smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

fn worm_smax(a: f32, b: f32, k: f32) -> f32 {
    return -worm_smin(-a, -b, k);
}

fn worm_sphere(p: vec3<f32>, c: vec3<f32>, r: f32) -> f32 {
    return length(p - c) - r;
}

// A good approximation, not an exact distance: step under it in `w_trace`.
fn worm_ellipsoid(p: vec3<f32>, c: vec3<f32>, r: vec3<f32>) -> f32 {
    let q = (p - c) / r;
    let k0 = length(q);
    let k1 = length(q / r);
    return k0 * (k0 - 1.0) / max(k1, 1.0e-6);
}

// Inigo Quilez's exact round cone: the workhorse for every tapered limb, the
// spine and the snout. Two radii and a segment are enough to describe a worm.
fn worm_round_cone(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, r1: f32, r2: f32) -> f32 {
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

// The rest-pose hands, in model units. The game replaces them with the animated
// hands, but the arms are built around the same pose, so the sheet and the game
// agree. Pasted from `worms.rs`, which is where the animation uses them.
const WORM_HAND_R = $HAND_R;
const WORM_HAND_L = $HAND_L;

const WORM_EYE_L = vec3<f32>(-0.175, 1.300, 0.520);
const WORM_EYE_R = vec3<f32>( 0.175, 1.300, 0.520);
const WORM_EYE_RADIUS: f32 = 0.062;
const WORM_NOSTRIL_L = vec3<f32>(-0.033, 1.180, 0.735);
const WORM_NOSTRIL_R = vec3<f32>( 0.033, 1.180, 0.735);

fn worm_mouth(p: vec3<f32>) -> f32 {
    // A wide grin, cut into the snout. The lower lip below it hides the bottom
    // of the ellipse; the tongue fills the middle.
    return worm_ellipsoid(p, vec3<f32>(0.0, 1.100, 0.710), vec3<f32>(0.110, 0.040, 0.055));
}

fn worm_tongue(p: vec3<f32>) -> f32 {
    return worm_ellipsoid(p, vec3<f32>(0.0, 1.082, 0.725), vec3<f32>(0.060, 0.018, 0.028));
}

fn worm_lower_lip(p: vec3<f32>) -> f32 {
    return worm_ellipsoid(p, vec3<f32>(0.0, 1.070, 0.710), vec3<f32>(0.088, 0.020, 0.038));
}

fn worm_socket(p: vec3<f32>) -> f32 {
    var d = worm_ellipsoid(p, WORM_EYE_L, vec3<f32>(0.078, 0.078, 0.070));
    d = min(d, worm_ellipsoid(p, WORM_EYE_R, vec3<f32>(0.078, 0.078, 0.070)));
    return d;
}

fn worm_brow(p: vec3<f32>) -> f32 {
    // 12's grumpy brow: inner end high, outer end low.
    var d = worm_round_cone(p, vec3<f32>(-0.060, 1.365, 0.535), vec3<f32>(-0.250, 1.350, 0.485), 0.030, 0.020);
    d = min(d, worm_round_cone(p, vec3<f32>(0.060, 1.365, 0.535), vec3<f32>(0.250, 1.350, 0.485), 0.030, 0.020));
    return d;
}

fn worm_eyelid(p: vec3<f32>) -> f32 {
    var d = worm_ellipsoid(p, vec3<f32>(-0.175, 1.347, 0.535), vec3<f32>(0.080, 0.013, 0.062));
    d = min(d, worm_ellipsoid(p, vec3<f32>( 0.175, 1.347, 0.535), vec3<f32>(0.080, 0.013, 0.062)));
    return d;
}

fn worm_arms(p: vec3<f32>, hand_r: vec3<f32>, hand_l: vec3<f32>) -> f32 {
    // The hands are the only moving parts: recoil and the whip pull the right
    // hand, and the arm follows. The left stays on the foregrip.
    let hr = hand_r;
    let hl = hand_l;
    var d = worm_round_cone(p, vec3<f32>(0.230, 0.980, 0.180), vec3<f32>(0.345, 0.910, 0.370), 0.098, 0.076);
    d = worm_smin(d, worm_round_cone(p, vec3<f32>(0.345, 0.910, 0.370), hr + vec3<f32>(0.007, 0.007, -0.022), 0.076, 0.058), 0.018);
    d = worm_smin(d, worm_sphere(p, hr, 0.086), 0.018);

    var l = worm_round_cone(p, vec3<f32>(-0.230, 0.980, 0.180), vec3<f32>(-0.335, 0.915, 0.375), 0.098, 0.076);
    l = worm_smin(l, worm_round_cone(p, vec3<f32>(-0.335, 0.915, 0.375), hl + vec3<f32>(-0.015, 0.012, -0.012), 0.076, 0.055), 0.018);
    l = worm_smin(l, worm_sphere(p, hl, 0.078), 0.018);
    return min(d, l);
}

fn worm_body(p: vec3<f32>, hand_r: vec3<f32>, hand_l: vec3<f32>) -> f32 {
    // Spine: hips on the ground, chest forward, head well in front. A C.
    var d = worm_round_cone(p, vec3<f32>(0.0, 0.280, -0.080), vec3<f32>(0.0, 0.520, 0.030), 0.220, 0.240);
    d = worm_smin(d, worm_round_cone(p, vec3<f32>(0.0, 0.520, 0.030), vec3<f32>(0.0, 0.800, 0.140), 0.240, 0.230), 0.055);
    d = worm_smin(d, worm_round_cone(p, vec3<f32>(0.0, 0.800, 0.140), vec3<f32>(0.0, 1.020, 0.260), 0.230, 0.180), 0.050);
    d = worm_smin(d, worm_round_cone(p, vec3<f32>(0.0, 1.020, 0.260), vec3<f32>(0.0, 1.120, 0.300), 0.180, 0.180), 0.035);

    // Skull, cheeks, snout.
    d = worm_smin(d, worm_ellipsoid(p, vec3<f32>(0.0, 1.270, 0.350), vec3<f32>(0.250, 0.240, 0.240)), 0.038);
    d = worm_smin(d, worm_sphere(p, vec3<f32>(-0.130, 1.210, 0.400), 0.130), 0.036);
    d = worm_smin(d, worm_sphere(p, vec3<f32>( 0.130, 1.210, 0.400), 0.130), 0.036);
    d = worm_smin(d, worm_round_cone(p, vec3<f32>(0.0, 1.200, 0.410), vec3<f32>(0.0, 1.140, 0.640), 0.170, 0.090), 0.032);
    d = worm_smin(d, worm_ellipsoid(p, vec3<f32>(0.0, 1.130, 0.680), vec3<f32>(0.090, 0.080, 0.075)), 0.022);
    d = worm_smin(d, worm_ellipsoid(p, vec3<f32>(0.0, 1.020, 0.520), vec3<f32>(0.115, 0.060, 0.090)), 0.026);
    d = worm_smin(d, worm_lower_lip(p), 0.015);

    // Sockets, grumpy brows, thin lids.
    d = worm_smax(d, -worm_socket(p), 0.010);
    d = worm_smin(d, worm_brow(p), 0.020);
    d = worm_smin(d, worm_eyelid(p), 0.012);

    // Cut grin with a tongue.
    d = worm_smax(d, -worm_mouth(p), 0.011);
    d = min(d, worm_tongue(p));

    // Tail: lies on the ground and curls up at the tip.
    var t = worm_round_cone(p, vec3<f32>(0.0, 0.280, -0.100), vec3<f32>(0.050, 0.150, -0.320), 0.160, 0.110);
    t = worm_smin(t, worm_round_cone(p, vec3<f32>(0.050, 0.150, -0.320), vec3<f32>(0.160, 0.090, -0.500), 0.110, 0.070), 0.028);
    t = worm_smin(t, worm_round_cone(p, vec3<f32>(0.160, 0.090, -0.500), vec3<f32>(0.240, 0.080, -0.620), 0.070, 0.045), 0.022);
    t = worm_smin(t, worm_round_cone(p, vec3<f32>(0.240, 0.080, -0.620), vec3<f32>(0.180, 0.120, -0.700), 0.045, 0.025), 0.018);
    d = worm_smin(d, t, 0.035);
    d = worm_smin(d, worm_arms(p, hand_r, hand_l), 0.030);

    // Eyes: hard union, so they stay round in their sockets.
    d = min(d, worm_sphere(p, WORM_EYE_L, WORM_EYE_RADIUS));
    d = min(d, worm_sphere(p, WORM_EYE_R, WORM_EYE_RADIUS));
    return d;
}

fn worm_normal(p: vec3<f32>, hand_r: vec3<f32>, hand_l: vec3<f32>) -> vec3<f32> {
    let e = 0.0015;
    return normalize(vec3<f32>(
        worm_body(p + vec3<f32>(e, 0.0, 0.0), hand_r, hand_l) - worm_body(p - vec3<f32>(e, 0.0, 0.0), hand_r, hand_l),
        worm_body(p + vec3<f32>(0.0, e, 0.0), hand_r, hand_l) - worm_body(p - vec3<f32>(0.0, e, 0.0), hand_r, hand_l),
        worm_body(p + vec3<f32>(0.0, 0.0, e), hand_r, hand_l) - worm_body(p - vec3<f32>(0.0, 0.0, e), hand_r, hand_l),
    ));
}

// The skin is a parameter so the sheet can keep its fixed colour and the game
// can use the player's. The dark and belly tints are passed separately: the
// game derives them from the skin, the sheet uses its own palette.
fn worm_skin(
    p: vec3<f32>,
    n: vec3<f32>,
    skin: vec3<f32>,
    skin_dark: vec3<f32>,
    belly: vec3<f32>,
    hand_r: vec3<f32>,
    hand_l: vec3<f32>,
) -> vec3<f32> {
    // Black bead eyes, one catchlight. That is the Liero read; big white
    // human eyes made the model a baby.
    let dl = length(p - WORM_EYE_L);
    let dr = length(p - WORM_EYE_R);
    if (min(abs(dl - WORM_EYE_RADIUS), abs(dr - WORM_EYE_RADIUS)) < 0.003) {
        let c = select(WORM_EYE_R, WORM_EYE_L, dl < dr);
        let dir = normalize(p - c);
        let hl = normalize(vec3<f32>(-0.35, 0.55, 0.75));
        if (dot(dir, hl) > 0.940) { return vec3<f32>(1.0); }
        return vec3<f32>(0.045, 0.055, 0.085);
    }

    // Cut grin: dark inside, with the tongue.
    if (worm_mouth(p) < 0.018) {
        if (worm_tongue(p) < 0.006) { return vec3<f32>(0.93, 0.42, 0.45); }
        return vec3<f32>(0.30, 0.075, 0.095);
    }

    var c = skin;
    // Cream belly, painted by the local normal: no second capsule, no seam.
    let belly_x = smoothstep(0.30, 0.12, abs(p.x));
    let belly_y = smoothstep(1.04, 0.94, p.y) * smoothstep(0.28, 0.62, n.z);
    let belly_m = belly_x * belly_y;
    c = mix(c, belly, belly_m);

    // Faint segment bands on the belly only: a worm cue with no extra geometry.
    let band = abs(fract((p.y - 0.25) * 5.5) - 0.5);
    let crease = smoothstep(0.40, 0.47, band) * belly_m;
    c = mix(c, belly * 0.88, crease * 0.55);

    // Four or five shallow rings down the body and tail, perpendicular to the
    // spine. Without them the long body reads as a slug.
    let seg = abs(fract((p.y * 0.9 - p.z * 0.7) * 5.0) - 0.5);
    let ring = smoothstep(0.44, 0.49, seg) * smoothstep(1.02, 0.94, p.y);
    c *= 1.0 - 0.10 * ring * (1.0 - belly_m);

    // A lighter underside on the snout, and two dark nostrils at the tip.
    let muzzle = smoothstep(1.25, 1.16, p.y) * smoothstep(0.48, 0.58, p.z) * smoothstep(0.94, 1.01, p.y);
    c = mix(c, belly, muzzle * 0.60);
    if ((length(p - WORM_NOSTRIL_L) < 0.013 || length(p - WORM_NOSTRIL_R) < 0.013) && p.z > 0.690) {
        c = vec3<f32>(0.70, 0.27, 0.25);
    }
    if (worm_brow(p) < 0.010) { c *= 0.86; }
    if (worm_eyelid(p) < 0.008) { c *= 0.94; }
    // Hands read as separate masses through value, not seams.
    if (worm_arms(p, hand_r, hand_l) < 0.008 && p.z > 0.55) { c = mix(c, skin_dark, 0.45); }
    if (p.y < 0.10) { c = mix(c, skin_dark, 0.25); }
    return c;
}
"#;

// The game's view of the shared sculpt: the hands come from the per-worm
// uniform, and the skin colour is the player's. Everything else (the body, the
// face, the material) is `WORM_SCULPT`, so the studio sheet cannot drift from
// what ships.
const WORM_TRACE: &str = r#"
fn w_map(p: vec3<f32>, w: Worm) -> f32 {
    return worm_body(p, w.hand_r.xyz, w.hand_l.xyz);
}

fn w_normal(p: vec3<f32>, w: Worm) -> vec3<f32> {
    return worm_normal(p, w.hand_r.xyz, w.hand_l.xyz);
}

fn w_colour(p: vec3<f32>, n: vec3<f32>, w: Worm) -> vec3<f32> {
    let skin = w.skin.rgb;
    return worm_skin(
        p,
        n,
        skin,
        skin * 0.84,
        mix(skin, vec3<f32>(1.0, 0.93, 0.82), 0.55),
        w.hand_r.xyz,
        w.hand_l.xyz);
}

// The world -> model transform. The CPU bakes the rig and the model's 1.8 m
// scale into these three axes and inverse scales, so this stays a few dots.
fn w_local(p: vec3<f32>, w: Worm) -> vec3<f32> {
    let q = p - w.pivot.xyz;
    return vec3<f32>(
        dot(q, w.right.xyz) * w.right.w,
        dot(q, w.up.xyz) * w.up.w + w.pivot.w,
        dot(q, w.face.xyz) * w.face.w);
}

fn w_local_dir(rd: vec3<f32>, w: Worm) -> vec3<f32> {
    return vec3<f32>(
        dot(rd, w.right.xyz) * w.right.w,
        dot(rd, w.up.xyz) * w.up.w,
        dot(rd, w.face.xyz) * w.face.w);
}

fn w_ray(ro: vec3<f32>, rd: vec3<f32>, c: vec3<f32>, r: f32) -> vec2<f32> {
    let oc = ro - c;
    let b = dot(oc, rd);
    let q = dot(oc, oc) - r * r;
    let h = b * b - q;
    if (h < 0.0) { return vec2<f32>(1.0, -1.0); }
    let s = sqrt(h);
    return vec2<f32>(-b - s, -b + s);
}

fn w_trace(ro: vec3<f32>, rd: vec3<f32>, w: Worm, t0: f32, t1: f32) -> f32 {
    let rol = w_local(ro, w);
    let rdl = w_local_dir(rd, w);
    let inv = 1.0 / max(length(rdl), 1.0e-5);
    var t = t0;
    for (var i = 0; i < 80; i++) {
        let d = w_map(rol + rdl * t, w);
        if (d < 0.0008) { return t; }
        t += max(d * 0.85 * inv, 0.0008 * inv);
        if (t > t1) { break; }
    }
    return -1.0;
}
"#;

const MARCH: &str = r#"
const VOX: f32 = $VOX;
const BAND: f32 = $BAND;
const CHUNK_M: f32 = $CHUNK_M;
const DIMS: vec3<f32> = vec3<f32>($DIMS);
const WORLD_M: vec3<f32> = vec3<f32>($WORLD_M);
const CHUNKS: vec3<i32> = vec3<i32>($CHUNKS);

// The terrain: one signed byte per voxel (value / 127 * BAND = metres, + in air).
// The hardware's trilinear filter does the interpolation Grid::sample does.
@group(0) @binding(1) var sdf_tex: texture_3d<f32>;
@group(0) @binding(2) var sdf_smp: sampler;
// One byte per chunk: how many chunks away (chessboard distance) the nearest
// chunk that holds a surface is. 0 = holds one. See `coarse_map` in renderer.rs.
@group(0) @binding(3) var coarse_tex: texture_3d<u32>;

// Distance to the terrain in metres (negative inside rock). A voxel's value
// sits at its integer coordinate, a texel's centre at +0.5, hence the shift.
fn map(p: vec3<f32>) -> f32 {
    let uvw = (p / VOX + 0.5) / DIMS;
    return textureSampleLevel(sdf_tex, sdf_smp, uvw, 0.0).r * BAND;
}

// Optimisation: empty-space skipping. Plain sphere tracing through open air
// advances at most BAND (1 m) per step, so a cave 60 m across costs 60+
// texture fetches per pixel. The coarse map says "no surface within k chunks",
// which lets the ray jump (k-1) chunks at once. One VOX is subtracted because
// the cell between a surface chunk and its neighbour belongs to the neighbour.
//
// Optimisation: the hit tolerance is `eps + t * slope`. With `slope` = the
// world size of half a pixel per metre of distance, a ray stops as soon as it
// is closer to the surface than the screen can show. Far-away grazing rays,
// the slowest ones, finish in a fraction of the steps.
const RELAX: f32 = 1.4;

fn trace(ro: vec3<f32>, rd: vec3<f32>, t_start: f32, t_end: f32, max_steps: i32, eps: f32, slope: f32) -> f32 {
    var t = t_start;
    var d = 1.0;
    for (var i = 0; i < max_steps; i++) {
        if (t >= t_end) { return -1.0; }
        let p = ro + rd * t;
        let c = clamp(vec3<i32>(floor(p / CHUNK_M)), vec3<i32>(0), CHUNKS - vec3<i32>(1));
        let k = textureLoad(coarse_tex, c, 0).r;
        if (k >= 2u) {
            t += (f32(k) - 1.0) * CHUNK_M - VOX;
            continue;
        }
        d = map(p);
        let tol = eps + t * slope;
        if (d < tol) {
            // The last step may have ended slightly inside the rock (negative d).
            // The field is nearly linear there, so adding d walks back to zero.
            var tt = t + d;
            tt += map(ro + rd * tt);
            return max(tt, t_start);
        }
        // The step is the distance the field promises is free, but never less
        // than the tolerance, so grazing rays cannot crawl.
        t += max(d * RELAX, tol);
    }
    // Out of steps: a ray this close to the surface is a grazing hit.
    if (d < 0.25) { return t; }
    return -1.0;
}

// Angle one pixel of the G-buffer spans (radians, small-angle).
fn pix_angle() -> f32 {
    return 2.0 * length(u.cam_up.xyz) / u.size.y;
}

fn normal_at(p: vec3<f32>) -> vec3<f32> {
    // Tetrahedron gradient: 4 samples instead of 6. The offset is wider than
    // a voxel on purpose: a trilinear field has a different gradient in every
    // cell, and sampling across cells blends those facets away.
    let e = VOX * 0.9;
    let k1 = vec3<f32>(1.0, -1.0, -1.0);
    let k2 = vec3<f32>(-1.0, -1.0, 1.0);
    let k3 = vec3<f32>(-1.0, 1.0, -1.0);
    let k4 = vec3<f32>(1.0, 1.0, 1.0);
    return normalize(k1 * map(p + k1 * e) + k2 * map(p + k2 * e) + k3 * map(p + k3 * e) + k4 * map(p + k4 * e));
}

// Ray against the world's bounding box: (t_enter, t_exit, exit axis as 0/1/2).
fn box_hit(ro: vec3<f32>, rd: vec3<f32>) -> vec3<f32> {
    let safe = select(rd, vec3<f32>(1.0e-6), abs(rd) < vec3<f32>(1.0e-6));
    let inv = 1.0 / safe;
    let ta = (vec3<f32>(0.0) - ro) * inv;
    let tb = (WORLD_M - ro) * inv;
    let tmn = min(ta, tb);
    let tmx = max(ta, tb);
    let t_near = max(max(tmn.x, tmn.y), tmn.z);
    let t_far = min(min(tmx.x, tmx.y), tmx.z);
    var axis = 0.0;
    if (tmx.y <= tmx.x && tmx.y <= tmx.z) { axis = 1.0; }
    if (tmx.z <= tmx.x && tmx.z <= tmx.y) { axis = 2.0; }
    return vec3<f32>(t_near, t_far, axis);
}

// Ray against a capsule: the distance along the ray to the first hit, or -1.
// Closed form (no marching): solve the ray against the infinite cylinder around
// the segment, and if the hit lies beyond the segment's ends, against the sphere
// at that end instead.
//
// Optimisation: almost every ray misses almost every capsule, and the closed
// form is expensive (divisions, square roots, ~60 operations). So first test the
// ray against a sphere around the whole capsule (`rb`, centred on the middle):
// that rejects a miss in a handful of operations.
fn cap_hit(ro: vec3<f32>, rd: vec3<f32>, pa: vec3<f32>, pb: vec3<f32>, ra: f32, rb: f32) -> f32 {
    let mid = (pa + pb) * 0.5;
    let om = ro - mid;
    let bm = dot(om, rd);
    let cm = dot(om, om) - rb * rb;
    if (bm * bm < cm || (bm > 0.0 && cm > 0.0)) { return -1.0; }   // misses, or is behind
    let ba = pb - pa;
    let oa = ro - pa;
    let baba = dot(ba, ba);
    let bard = dot(ba, rd);
    let baoa = dot(ba, oa);
    let rdoa = dot(rd, oa);
    let oaoa = dot(oa, oa);
    let a = baba - bard * bard;
    var b = baba * rdoa - baoa * bard;
    var c = baba * oaoa - baoa * baoa - ra * ra * baba;
    var h = b * b - a * c;
    if (h >= 0.0) {
        let t = (-b - sqrt(h)) / a;
        let y = baoa + t * bard;   // where along the segment (0 = a, baba = b)
        if (y > 0.0 && y < baba) { return t; }
        let oc = select(ro - pb, oa, y <= 0.0);   // the end sphere on that side
        b = dot(rd, oc);
        c = dot(oc, oc) - ra * ra;
        h = b * b - c;
        if (h > 0.0) { return -b - sqrt(h); }
    }
    return -1.0;
}

fn cap_normal(p: vec3<f32>, pa: vec3<f32>, pb: vec3<f32>) -> vec3<f32> {
    let ba = pb - pa;
    let k = clamp(dot(p - pa, ba) / dot(ba, ba), 0.0, 1.0);
    return normalize(p - (pa + ba * k));
}

// The other two primitives, then a dispatcher over all three. A cylinder and a
// capsule share the ray/cylinder solve; a cylinder just ends in flat discs
// instead of spheres. A cuboid is a slab test in its own (forward-aligned) frame.

fn box_axes(fwd: vec3<f32>) -> mat3x3<f32> {
    let z = normalize(fwd);
    var x = cross(vec3<f32>(0.0, 1.0, 0.0), z);
    if (dot(x, x) < 0.001) { x = vec3<f32>(1.0, 0.0, 0.0); } else { x = normalize(x); }
    let y = cross(z, x);
    return mat3x3<f32>(x, y, z);
}

fn cyl_hit(ro: vec3<f32>, rd: vec3<f32>, pa: vec3<f32>, pb: vec3<f32>, ra: f32, rb: f32) -> f32 {
    let mid = (pa + pb) * 0.5;
    let om = ro - mid;
    let bm = dot(om, rd);
    let cm = dot(om, om) - rb * rb;
    if (bm * bm < cm || (bm > 0.0 && cm > 0.0)) { return -1.0; }
    let ba = pb - pa;
    let baba = dot(ba, ba);
    let bard = dot(ba, rd);
    let oa = ro - pa;
    let baoa = dot(ba, oa);
    let rdoa = dot(rd, oa);
    let oaoa = dot(oa, oa);
    let a = baba - bard * bard;
    let b = baba * rdoa - baoa * bard;
    let c = baba * oaoa - baoa * baoa - ra * ra * baba;
    var best = -1.0;
    let h = b * b - a * c;
    if (h >= 0.0 && a > 1.0e-9) {
        let sq = sqrt(h);
        let inv = 1.0 / a;
        var t = (-b - sq) * inv;
        var y = baoa + t * bard;
        if (t > 0.0 && y > 0.0 && y < baba) { best = t; }
        t = (-b + sq) * inv;
        y = baoa + t * bard;
        if (t > 0.0 && y > 0.0 && y < baba && (best < 0.0 || t < best)) { best = t; }
    }
    if (abs(bard) > 1.0e-6) {
        var t = -baoa / bard;
        if (t > 0.0 && (best < 0.0 || t < best)) {
            let q = oa + rd * t;
            if (dot(q, q) - (dot(q, ba) * dot(q, ba)) / baba <= ra * ra) { best = t; }
        }
        t = (baba - baoa) / bard;
        if (t > 0.0 && (best < 0.0 || t < best)) {
            let q = oa + rd * t;
            if (dot(q, q) - (dot(q, ba) * dot(q, ba)) / baba <= ra * ra) { best = t; }
        }
    }
    return best;
}

fn cuboid_hit(ro: vec3<f32>, rd: vec3<f32>, centre: vec3<f32>, half: vec3<f32>, fwd: vec3<f32>, rb: f32) -> f32 {
    let om = ro - centre;
    let bm = dot(om, rd);
    let cm = dot(om, om) - rb * rb;
    if (bm * bm < cm || (bm > 0.0 && cm > 0.0)) { return -1.0; }
    let mt = transpose(box_axes(fwd));
    let ol = mt * (ro - centre);
    let dl = mt * rd;
    var tmin = -1.0e9;
    var tmax = 1.0e9;
    for (var i = 0; i < 3; i++) {
        if (abs(dl[i]) < 1.0e-6) {
            if (abs(ol[i]) > half[i]) { return -1.0; }
        } else {
            let inv = 1.0 / dl[i];
            let t1 = (-half[i] - ol[i]) * inv;
            let t2 = (half[i] - ol[i]) * inv;
            tmin = max(tmin, min(t1, t2));
            tmax = min(tmax, max(t1, t2));
            if (tmin > tmax) { return -1.0; }
        }
    }
    if (tmax < 0.0) { return -1.0; }
    return max(tmin, 0.0);
}

fn cyl_normal(p: vec3<f32>, pa: vec3<f32>, pb: vec3<f32>, ra: f32) -> vec3<f32> {
    let ba = pb - pa;
    let len = length(ba);
    let axis = ba / len;
    let along = dot(p - pa, axis);
    let radial = (p - pa) - axis * along;
    if (length(radial) < ra * 0.999) {
        return select(-axis, axis, along > len * 0.5);
    }
    return radial / length(radial);
}

fn cuboid_normal(p: vec3<f32>, centre: vec3<f32>, fwd: vec3<f32>, half: vec3<f32>) -> vec3<f32> {
    let m = box_axes(fwd);
    let q = transpose(m) * (p - centre);
    let dx = half.x - abs(q.x);
    let dy = half.y - abs(q.y);
    let dz = half.z - abs(q.z);
    if (dx < dy && dx < dz) { return m[0] * sign(q.x); }
    if (dy < dz) { return m[1] * sign(q.y); }
    return m[2] * sign(q.z);
}

fn prim_hit(ro: vec3<f32>, rd: vec3<f32>, i: i32) -> f32 {
    let p = u.caps[i];
    let k = i32(p.col.w + 0.5);
    if (k == 0) { return cap_hit(ro, rd, p.a.xyz, p.b.xyz, p.a.w, p.b.w); }
    if (k == 1) { return cyl_hit(ro, rd, p.a.xyz, p.b.xyz, p.a.w, p.b.w); }
    return cuboid_hit(ro, rd, p.a.xyz, p.b.xyz, p.c.xyz, p.b.w);
}

fn prim_normal(p: vec3<f32>, i: i32) -> vec3<f32> {
    let pr = u.caps[i];
    let k = i32(pr.col.w + 0.5);
    if (k == 0) { return cap_normal(p, pr.a.xyz, pr.b.xyz); }
    if (k == 1) { return cyl_normal(p, pr.a.xyz, pr.b.xyz, pr.a.w); }
    return cuboid_normal(p, pr.a.xyz, pr.c.xyz, pr.b.xyz);
}

// Does a shadow caster block this ray? (For shadows on the terrain. The casters
// are the first `counts.y` primitives.)
fn caps_block(ro: vec3<f32>, rd: vec3<f32>) -> bool {
    for (var i = 0; i < i32(u.counts.y); i++) {
        if (prim_hit(ro, rd, i) > 0.0) { return true; }
    }
    return false;
}

// The palette below is written the way you would pick colours in a paint
// program (sRGB). Shading happens in linear light, so convert on the way in:
// without this everything looks washed out.
fn sky(rd: vec3<f32>) -> vec3<f32> {
    let h = clamp(rd.y, 0.0, 1.0);
    var c = mix(srgb(vec3<f32>(0.95, 0.88, 0.74)), srgb(vec3<f32>(0.40, 0.60, 0.88)), pow(h, 0.45));
    let s = max(dot(rd, u.sun.xyz), 0.0);
    c += vec3<f32>(1.0, 0.85, 0.55) * (pow(s, 800.0) * 2.0 + pow(s, 14.0) * 0.18);
    return c;
}

fn albedo(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    // Layered rock: soft strata that wobble with noise, so a fresh crater wall
    // shows bands like a cutaway. Grass where the ground faces up.
    let wob = vnoise3(p * vec3<f32>(0.3, 0.6, 0.3));
    let strata = sin(p.y * 1.3 + wob * 3.5) * 0.5 + 0.5;
    let rock = mix(srgb(vec3<f32>(0.52, 0.42, 0.36)), srgb(vec3<f32>(0.66, 0.54, 0.43)), strata * 0.6);
    let speck = vnoise3(p * 3.0);
    let grass = mix(srgb(vec3<f32>(0.40, 0.60, 0.26)), srgb(vec3<f32>(0.58, 0.72, 0.30)), speck);
    let up = smoothstep(0.55, 0.78, n.y + (wob - 0.5) * 0.35);
    return mix(rock, grass, up);
}

// How much soot covers this point, 0..1. Summed from the recent blasts so two
// overlapping craters are darker than one.
fn soot_at(p: vec3<f32>) -> f32 {
    var s = 0.0;
    for (var i = 0; i < i32(u.counts.z); i++) {
        let S = u.soot[i];
        if (S.b.x <= 0.001) { continue; }
        let f = 1.0 - smoothstep(S.a.w * 0.55, S.a.w, length(p - S.a.xyz));
        s = max(s, f * S.b.x);
    }
    return min(s, 1.0);
}

// The world is deliberately quieter than the actors. Most of the green is
// pulled out of rock, grass, sky and the arena wall, so the weapon palette,
// blood and health pips are the only strong colour on screen. Capsules are
// drawn after this and are never muted.

struct Out {
    @location(0) color: vec4<f32>,
    @location(1) gbuf: vec4<f32>,   // xyz: normal * 0.5 + 0.5, w: distance along the ray
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One big triangle covering the screen: no vertex buffer, no seam.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> Out {
    let uv = frag.xy / u.size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let ro = u.cam_pos.xyz;
    let rd = normalize(u.cam_fwd.xyz + ndc.x * u.cam_right.xyz + ndc.y * u.cam_up.xyz);

    var kind = 0;   // 0 sky, 1 terrain, 2 arena wall, 3 capsule, 4 worm SDF
    // The nearest capsule on this ray. Done first because it also shortens the
    // terrain march: no need to look for rock behind a worm. Only the capsules
    // whose bit is set for this pixel's tile are tested, usually none.
    var tc = 1.0e9;
    var ci = -1;
    let tile = min(vec2<u32>(uv * vec2<f32>(32.0, 18.0)), vec2<u32>(31u, 17u));
    let tile_i = tile.y * 32u + tile.x;
    let lane = tile_i & 3u;
    let group = tile_i >> 2u;
    // $WORDS words: capsules 0..31, then 32..63, and so on. Word w starts
    // $GROUPS vec4s into the tile array (see `tile_masks` in renderer.rs).
    for (var w = 0u; w < $WORDSu; w += 1u) {
        var mask = u.tiles[group + w * $GROUPSu][lane];
        while (mask != 0u) {
            let i = i32(firstTrailingBit(mask)) + i32(w * 32u);
            mask = mask & (mask - 1u);   // clear the lowest set bit
            let flags = i32(u.caps[i].c.w + 0.5);
            if ((flags & 4) != 0) { continue; } // shadow proxy: not drawn
            let h = prim_hit(ro, rd, i);
            if (h > 0.0 && h < tc) { tc = h; ci = i; }
        }
    }

    // Worm bodies are traced separately: one bounding-sphere test per slot, and
    // only the pixels inside pay for the SDF. A max of four is plenty.
    var tw = 1.0e9;
    var wi = -1;
    for (var i = 0; i < $WORMS; i++) {
        let w = u.worms[i];
        if (w.bound.w <= 0.0) { continue; }
        let b = w_ray(ro, rd, w.bound.xyz, w.bound.w);
        let start = max(b.x, 0.0);
        if (b.y <= start) { continue; }
        let h = w_trace(ro, rd, w, start, min(b.y, tc));
        if (h > 0.0 && h < tw) { tw = h; wi = i; }
    }
    let nearest = min(tc, tw);
    var t = 400.0;
    var n = vec3<f32>(0.0);

    let bx = box_hit(ro, rd);
    let t_start = max(bx.x, 0.0);
    let inside = all(ro >= vec3<f32>(0.0)) && all(ro <= WORLD_M);
    if (bx.y > t_start) {
        if (inside && map(ro) < 0.0) {
            // The camera is buried in rock: paint it solid rather than showing
            // the far side of some cavity.
            kind = 2;
            t = 0.01;
            n = -rd;
        } else {
            let th = trace(ro, rd, t_start, min(bx.y, nearest), 160, 0.003, 0.5 * pix_angle());
            if (th >= 0.0) {
                kind = 1;
                t = th;
            } else if (tc <= tw && ci >= 0 && tc < bx.y) {
                kind = 3;   // the march gave up at the capsule: nothing nearer
                t = tc;
            } else if (wi >= 0 && tw < bx.y) {
                kind = 4;
                t = tw;
            } else if (!(bx.z == 1.0 && rd.y > 0.0)) {
                // Left through a side or the floor: that is the closed arena's wall.
                // (Out through the top is open sky, purely for looks.)
                kind = 2;
                t = bx.y;
                var wn = vec3<f32>(0.0);
                if (bx.z == 0.0) { wn.x = -sign(rd.x); }
                if (bx.z == 1.0) { wn.y = -sign(rd.y); }
                if (bx.z == 2.0) { wn.z = -sign(rd.z); }
                n = wn;
            }
        }
    }

    var col = sky(rd);
    var inkless = false;
    // How much this pixel is its own light source, 0..1. Written to the colour
    // target's alpha so the bloom in `fs_present` only glows emissive things,
    // never the bright sky or pale rock. A capsule drawn self-lit sets it.
    var emit = 0.0;
    let fog_col = srgb(vec3<f32>(0.93, 0.86, 0.72));
    if (kind == 1 || kind == 3 || kind == 4) {
        let p = ro + rd * t;
        var base = vec3<f32>(0.0);
        if (kind == 1) {
            n = normal_at(p);
            base = albedo(p, n);
        } else if (kind == 3) {
            n = prim_normal(p, ci);
            base = srgb(u.caps[ci].col.rgb);
        } else {
            // The body is one SDF: one normal, one material, one outline.
            let wp = u.worms[wi];
            let pl = w_local(p, wp);
            let nl = w_normal(pl, wp);
            n = normalize(wp.right.xyz * nl.x + wp.up.xyz * nl.y + wp.face.xyz * nl.z);
            base = srgb(w_colour(pl, nl, wp));
            base = mix(base, srgb(vec3<f32>(1.0, 0.90, 0.90)), wp.skin.w);
        }
        // Fresh craters are burnt: darken the albedo wherever a recent blast
        // left soot. This follows the bowl's walls for free, which decals on a
        // plane cannot.
        base *= 1.0 - soot_at(p) * 0.90;

        // Hard shadow ray towards the sun. Pattern: it reuses `trace`, so it
        // gets the empty-space skipping for free.
        var lit = max(dot(n, u.sun.xyz), 0.0);
        if (u.flags.y > 0.5 && lit > 0.0) {
            let so = p + n * 0.08;
            if (trace(so, u.sun.xyz, 0.0, 60.0, 128, 0.02, 0.002) >= 0.0) { lit = 0.0; }
            // Worms shadow the terrain (a shadow under the feet is what tells
            // you how high you are), but not themselves: that would speckle.
            if (kind == 1 && caps_block(so, u.sun.xyz)) { lit = 0.0; }
        }
        var occl = 1.0;
        if (u.flags.z > 0.5 && kind == 1) {
            // Cheap ambient occlusion: how far below "free space" is the field
            // a few steps out along the normal? (Terrain only: a capsule's
            // normal is meaningless to the terrain field, so skip the samples.)
            var o = 0.0;
            var w = 1.0;
            for (var i = 1; i <= 4; i++) {
                let h = 0.14 * f32(i);
                o += (h - map(p + n * h)) * w;
                w *= 0.6;
            }
            occl = clamp(1.0 - 1.6 * o, 0.0, 1.0);
        }

        let tint = mix(vec3<f32>(0.42, 0.45, 0.70), vec3<f32>(1.10, 1.0, 0.86), toon(lit));
        // Draw flags ride in `c.w` (see `pack` in renderer.rs): bit 0 self-lit,
        // bit 1 no ink outline. The SDF body has neither.
        let cap_flags = select(0, i32(u.caps[max(ci, 0)].c.w + 0.5), kind == 3);
        let self_lit = (cap_flags & 1) != 0;
        inkless = (cap_flags & 2) != 0;
        let to_cam = ro - p;
        let lamp_d = length(to_cam);
        if (self_lit) {
            // Brighter than white: the present pass tone-maps it and the bloom
            // pass spills the excess light into the surrounding rock. This is
            // what makes a bolt read as energy and not as a painted dot.
            col = base * 5.0;
            emit = 1.0;
        } else {
            col = base * tint * mix(0.55, 1.0, occl);

            // A warm lamp at the camera so caves and shadowed ground stay readable.
            // It only fills in where the sun does not reach (1 - toon(lit)), and
            // uses the same banding so it still looks drawn, not rendered.
            let lamp = max(dot(n, to_cam / lamp_d), 0.0) * (1.0 - smoothstep(3.0, 26.0, lamp_d));
            col += base * vec3<f32>(1.0, 0.78, 0.5) * toon(lamp) * 0.5 * (1.0 - toon(lit)) * mix(0.5, 1.0, occl);
            // Blasts, flashes and plasma light the world around them.
            col = lit_by_points(p, n, base, col);
        }

        // A glossy skin: capsules catch a small highlight and a cool rim, which
        // is what separates a "drawn" prop from flat toon bands. Terrain stays
        // matte. It is a few multiplications on worm pixels only.
        if ((kind == 3 || kind == 4) && !self_lit) {
            let v = to_cam / lamp_d;
            let h = normalize(u.sun.xyz + v);
            let spec = pow(max(dot(n, h), 0.0), 36.0) * lit;
            col += vec3<f32>(1.0, 0.96, 0.86) * spec * 0.45;
            let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0);
            col += vec3<f32>(0.55, 0.72, 1.0) * rim * 0.22;
        }

        // The brush: tint the terrain it would change and draw its rim.
        if (kind == 1 && u.brush.w > 0.0) {
            let dist = length(p - u.brush.xyz);
            let inside_brush = 1.0 - smoothstep(u.brush.w - 0.03, u.brush.w, dist);
            let ring = 1.0 - smoothstep(0.0, 0.07, abs(dist - u.brush.w));
            col = mix(col, srgb(vec3<f32>(1.0, 0.55, 0.18)), inside_brush * 0.25 + ring * 0.85);
        }
        col = mix(col, fog_col, 1.0 - exp(-t * 0.002));
    } else if (kind == 2) {
        col = mix(srgb(vec3<f32>(0.20, 0.22, 0.32)), srgb(vec3<f32>(0.36, 0.38, 0.52)), clamp(n.y * 0.5 + 0.5, 0.0, 1.0));
    }

    // Grade after fog and the brush tint, so the distance haze is muted too.
    // The sky keeps a little more colour: a grey sky reads as a bug.
    if (kind == 1 || kind == 2) {
        col = muted(col, u.fx.y);
    } else if (kind == 0) {
        col = muted(col, u.fx.y * 0.55);
    }

    var out: Out;
    out.color = vec4<f32>(col, emit);
    // An inkless primitive writes a sentinel normal instead of a real one:
    // `fs_ink` reads it as "draw no outline here" (and draws none around it).
    var gbuf_n = n * 0.5 + 0.5;
    if (inkless) {
        gbuf_n = vec3<f32>(-1.0);
    }
    out.gbuf = vec4<f32>(gbuf_n, t);
    return out;
}
"#;

const SPRITES: &str = r#"
// Soft billboards and decals: camera-facing quads layered over the inked
// picture. They are raster, not ray-marched, so a fireball is a soft sprite
// instead of an opaque ball, and a scorch is painted onto the rock instead of
// floating above it. The G-buffer supplies depth (occlusion) and, for decals,
// the exact surface point and normal. Blood is a decal too: the stain reads the
// G-buffer and shades itself like the rock it lies on, and a droplet is a
// small round bead. Both cost one quad, so the map can keep thousands of marks.
struct Sprite {
    a: vec4<f32>, // xyz centre, w radius (metres)
    b: vec4<f32>, // rgb colour (sRGB), w opacity 0..1
    c: vec4<f32>, // glow/smoke: x seed, y softness; decal: xyz normal, w seed
}
struct Sprites { data: array<Sprite> }
@group(0) @binding(1) var<storage, read> sprites: Sprites;
@group(0) @binding(2) var gbuf_tex: texture_2d<f32>;
// The terrain distance field. Blood only paints terrain, so a stain can ask
// "is this point on the rock?" instead of colouring a worm that stands in the
// puddle. Same encoding as `map` in the march shader.
@group(0) @binding(3) var sdf_tex: texture_3d<f32>;
@group(0) @binding(4) var sdf_smp: sampler;
const SDF_VOX: f32 = $VOX;
const SDF_BAND: f32 = $BAND;
const SDF_DIMS: vec3<f32> = vec3<f32>($DIMS);

fn terrain_distance(p: vec3<f32>) -> f32 {
    let uvw = (p / SDF_VOX + 0.5) / SDF_DIMS;
    return textureSampleLevel(sdf_tex, sdf_smp, uvw, 0.0).r * SDF_BAND;
}

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) i: u32,
    /// Clip-space position (-1..1). `pos` above is in framebuffer pixels in the
    /// fragment stage, so a sprite shader must use this to build a view ray.
    @location(2) ndc: vec2<f32>,
}

// The sRGB colour is converted to linear so the hardware re-encodes it to the
// same colour on the sRGB target (the march shader does the same).
fn lin(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

fn sprite_rd(ndc: vec2<f32>) -> vec3<f32> {
    return normalize(u.cam_fwd.xyz + ndc.x * u.cam_right.xyz + ndc.y * u.cam_up.xyz);
}

@vertex
fn vs_sprite(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VSOut {
    let s = sprites.data[ii];
    let right = normalize(u.cam_right.xyz);
    let up = normalize(u.cam_up.xyz);
    let fwd = normalize(u.cam_fwd.xyz);
    let tan = length(u.cam_up.xyz);
    let aspect = length(u.cam_right.xyz) / tan;
    let rel = s.a.xyz - u.cam_pos.xyz;
    let depth = dot(rel, fwd);
    let corner = vec2<f32>(
        select(-1.0, 1.0, (vi & 1u) != 0u),
        select(-1.0, 1.0, (vi & 2u) != 0u));
    var o: VSOut;
    o.local = corner;
    o.i = ii;
    if (depth < 0.05) {
        o.pos = vec4<f32>(0.0, 0.0, 2.0, 1.0); // clipped: behind the camera
        return o;
    }
    let cx = dot(rel, right) / (depth * tan * aspect);
    let cy = dot(rel, up) / (depth * tan);
    let sx = s.a.w / (depth * tan * aspect);
    let sy = s.a.w / (depth * tan);
    o.pos = vec4<f32>(cx + corner.x * sx, cy + corner.y * sy, 0.0, 1.0);
    o.ndc = o.pos.xy;
    return o;
}

// Fade a sprite that is behind the nearest surface, so a fireball behind a hill
// is occluded without a hard pop. The sprite is treated as a sphere: the depth
// tested is the near surface of that sphere along this pixel's ray, not the flat
// billboard's centre depth. Without the sphere a fireball sitting in its own
// crater is erased wholesale by the near rim, even the part that should rise
// above it.
fn occlude(ndc: vec2<f32>, centre: vec3<f32>, radius: f32, soft: f32) -> f32 {
    // NDC y points up, texture rows run down: flip y or every depth query reads
    // the scene at the mirrored pixel (the foreground below the sprite) and
    // hilariously hides it behind nothing.
    let px = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * u.size.xy;
    let t_scene = textureLoad(gbuf_tex, vec2<i32>(floor(px)), 0).w;
    let rd = sprite_rd(ndc);
    let rel = centre - u.cam_pos.xyz;
    let t_centre = dot(rel, rd);
    // Perpendicular distance from the ray to the sphere centre, then the near
    // root of the ray/sphere quadratic.
    let d2 = max(dot(rel, rel) - t_centre * t_centre, 0.0);
    let near = t_centre - sqrt(max(radius * radius - d2, 0.0));
    let fade = clamp(soft, 0.08, 0.35);
    return 1.0 - smoothstep(0.0, fade, near - t_scene);
}

@fragment
fn fs_glow(vin: VSOut) -> @location(0) vec4<f32> {
    let r = length(vin.local);
    if (r > 1.0) { discard; }
    let s = sprites.data[vin.i];
    let flags = i32(s.c.z + 0.5);
    var shape = pow(smoothstep(1.0, 0.0, r), 1.7);
    var tint = lin(s.b.rgb);
    if ((flags & 2) != 0) {
        // A shockwave: a ragged ring near the edge of the quad, not a disc.
        let band = max(s.c.w / s.a.w, 0.004);
        let ang = atan2(vin.local.y, vin.local.x);
        let ragg = vnoise2(vec2<f32>(ang * 3.0 + s.c.x, s.c.x * 0.7));
        shape = smoothstep(band * (0.55 + 0.9 * ragg), 0.0, abs(r - 0.80)) * (0.55 + 0.45 * ragg);
    } else if ((flags & 4) != 0) {
        // Fire: two octaves of noise tear the edge into tongues and break the
        // perfect circle, and a small white-hot core sits at the middle.
        let n1 = vnoise2(vin.local * 2.9 + vec2<f32>(s.c.x, s.c.x * 1.3));
        let n2 = vnoise2(vin.local * 7.1 - vec2<f32>(s.c.x * 0.7, s.c.x));
        let n3 = vnoise2(vin.local * 13.0 + vec2<f32>(s.c.x * 1.7, s.c.x * 0.3));
        let ragged = 0.5 * n1 + 0.3 * n2 + 0.2 * n3;
        // Cut holes out of the sheet of flame: that is what turns a disc into
        // tongues of fire. The holes are larger towards the edge.
        let holes = smoothstep(0.30, 0.46, ragged + (1.0 - r) * 0.30);
        shape = smoothstep(1.0, 0.45, r + (ragged - 0.5) * 0.55) * holes * (0.55 + 0.45 * (1.0 - r));
        tint = mix(tint, vec3<f32>(3.0, 2.2, 1.4), pow(max(1.0 - r, 0.0), 3.0) * 0.22);
    }
    var alpha = s.b.w * shape;
    if ((flags & 1) == 0) {
        alpha *= occlude(vin.ndc, s.a.xyz, s.a.w, s.c.y);
    }
    if (alpha <= 0.004) { discard; }
    // Fire is orange light with a small white heart; a flat 3x gain turned the
    // whole ball into a white blob. Sparks and flashes keep the hotter gain.
    let gain = select(3.0, 1.25, (flags & 4) != 0);
    return vec4<f32>(tint * alpha * gain, alpha);
}

@fragment
fn fs_smoke(vin: VSOut) -> @location(0) vec4<f32> {
    let r = length(vin.local);
    if (r > 1.0) { discard; }
    let s = sprites.data[vin.i];
    // A ragged edge, so a cloud reads as billowing smoke rather than a disc.
    let wob = vnoise2(vin.local * 3.2 + vec2<f32>(s.c.x, s.c.x * 0.7)) - 0.5;
    let edge = smoothstep(1.0, 0.15, r + wob * 0.28);
    var alpha = s.b.w * edge;
    if ((i32(s.c.z + 0.5) & 1) == 0) {
        alpha *= occlude(vin.ndc, s.a.xyz, s.a.w, s.c.y);
    }
    if (alpha <= 0.004) { discard; }
    return vec4<f32>(lin(s.b.rgb), alpha);
}

@fragment
fn fs_decal(vin: VSOut) -> @location(0) vec4<f32> {
    let s = sprites.data[vin.i];
    let ndc = vin.ndc;
    // NDC y is up, texture rows run down (see `occlude`).
    let px = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * u.size.xy;
    let g = textureLoad(gbuf_tex, vec2<i32>(floor(px)), 0);
    // Only paint onto solid surfaces: the sky keeps its far distance, and
    // inkless primitives mark their normal with -1.
    if (g.w > 300.0 || g.x < 0.0) { discard; }
    // The visible surface point, projected from the G-buffer. A stain is a
    // sphere of paint: every surface point inside it gets the colour. That is
    // what lets one stain cover a curved crater wall instead of a flat plane.
    let p = u.cam_pos.xyz + sprite_rd(ndc) * g.w;
    let t = p - s.a.xyz;
    let rr = length(t) / s.a.w;
    if (rr > 1.0) { discard; }
    // An irregular splatter edge, so a stain reads as splatter, not a stamp.
    let wob = vnoise3(t * 3.0 + vec3<f32>(s.c.w));
    let mask = smoothstep(1.0, 0.22, rr + (wob - 0.5) * 0.55);
    let alpha = s.b.w * mask;
    if (alpha <= 0.004) { discard; }
    return vec4<f32>(lin(s.b.rgb), alpha);
}

// Shade a blood mark with the same sun ramp, camera lamp, point lights, fog and
// world grade as the terrain. A stain is drawn over the inked picture, not into
// its albedo, so without this it would read as a bright sticker in a dark cave.
fn shade_blood(p: vec3<f32>, n: vec3<f32>, base: vec3<f32>) -> vec3<f32> {
    let lit = max(dot(n, u.sun.xyz), 0.0);
    let tint = mix(vec3<f32>(0.42, 0.45, 0.70), vec3<f32>(1.10, 1.0, 0.86), toon(lit));
    var col = base * tint;
    let to_cam = u.cam_pos.xyz - p;
    let d = length(to_cam);
    let lamp = max(dot(n, to_cam / max(d, 0.001)), 0.0) * (1.0 - smoothstep(3.0, 26.0, d));
    col += base * vec3<f32>(1.0, 0.78, 0.5) * toon(lamp) * 0.5 * (1.0 - toon(lit));
    col = lit_by_points(p, n, base, col);
    col = mix(col, srgb(vec3<f32>(0.93, 0.86, 0.72)), 1.0 - exp(-d * 0.002));
    return muted(col, u.fx.y);
}

@fragment
fn fs_blood(vin: VSOut) -> @location(0) vec4<f32> {
    let s = sprites.data[vin.i];
    // A stain carries a unit surface normal; a droplet has none. That one test
    // picks the mode without another buffer or pipeline.
    if (dot(s.c.xyz, s.c.xyz) < 0.5) {
        // A bead in flight: a round quad with a fake sphere normal, so it
        // catches the sun instead of reading as a flat red disc.
        let r = length(vin.local);
        if (r > 1.0) { discard; }
        var alpha = s.b.w * smoothstep(1.0, 0.72, r);
        alpha *= occlude(vin.ndc, s.a.xyz, s.a.w, 0.08);
        if (alpha <= 0.004) { discard; }
        let right = normalize(u.cam_right.xyz);
        let up = normalize(u.cam_up.xyz);
        let fwd = normalize(u.cam_fwd.xyz);
        let n = normalize(right * vin.local.x + up * vin.local.y - fwd * sqrt(max(1.0 - r * r, 0.0)));
        let lit = max(dot(n, u.sun.xyz), 0.0);
        // Self-lit enough to read in a dark crater, shaded enough to be a bead.
        return vec4<f32>(lin(s.b.rgb) * (0.65 + 0.85 * toon(lit)), alpha);
    }
    // A mark on the rock: find the visible surface under this pixel and paint
    // only the disc lying in that surface's plane. The plane test keeps a mark
    // on one side of a thin wall from bleeding through to the other side.
    let ndc = vin.ndc;
    let px = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * u.size.xy;
    let g = textureLoad(gbuf_tex, vec2<i32>(floor(px)), 0);
    if (g.w > 300.0 || g.x < 0.0) { discard; }
    let p = u.cam_pos.xyz + sprite_rd(ndc) * g.w;
    let n = normalize(g.xyz * 2.0 - 1.0);
    // Blood belongs to the rock. A worm, gun or rope standing in the puddle
    // is its own surface, more than a voxel away from the terrain field; leave
    // it clean. (The feet may still catch a little where they touch the ground,
    // which is what standing in blood should look like.)
    if (abs(terrain_distance(p)) > 0.22) { discard; }
    let d = p - s.a.xyz;
    let along = dot(d, n);
    if (abs(along) > 0.20) { discard; }
    let rr = length(d - n * along) / s.a.w;
    if (rr > 1.0) { discard; }
    let wob = vnoise2(vec2<f32>(d.x * 3.0 + d.z * 1.7, d.y * 3.0 + d.z * 0.9) + vec2<f32>(s.c.w));
    let mask = smoothstep(1.0, 0.22, rr + (wob - 0.5) * 0.55);
    let alpha = s.b.w * mask;
    if (alpha <= 0.004) { discard; }
    return vec4<f32>(shade_blood(p, n, lin(s.b.rgb)), alpha);
}
"#;

// Two fragment entry points share this module:
//   fs_ink     G-buffer resolution: finds the outlines and draws them over the
//              colour image into the `ink` texture.
//   fs_present screen resolution: upscales `ink` with a bilinear filter and adds
//              the paper grain, vignette and crosshair.
// Splitting them means the expensive edge detection runs on the (possibly
// reduced) internal resolution, so the automatic resolution scale cuts its
// cost too; only the cheap upscale pays for every screen pixel.
const POST: &str = r#"
@group(0) @binding(1) var color_tex: texture_2d<f32>;
@group(0) @binding(2) var gbuf_tex: texture_2d<f32>;
@group(0) @binding(3) var ink_tex: texture_2d<f32>;
@group(0) @binding(4) var ink_smp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn load_gb(p: vec2<i32>) -> vec4<f32> {
    let lim = vec2<i32>(u.size.xy) - vec2<i32>(1);
    return textureLoad(gbuf_tex, clamp(p, vec2<i32>(0), lim), 0);
}

// Ink where the picture has an edge, found in the G-buffer, not the colours:
//  - a depth edge is a silhouette. The second difference is ~0 on any flat
//    slanted wall, so walls seen at a grazing angle do not light up.
//  - a normal edge is a crease inside one object.
// Only the NEARER side of a silhouette draws the line, so lines hug the
// object instead of bleeding into the sky.
fn edge(p: vec2<i32>, o: i32) -> f32 {
    let c = load_gb(p);
    // Inkless primitives mark their G-buffer normal with -1: no outline on
    // them, and none drawn onto the rock behind them either.
    if (c.x < 0.0 || c.w > 300.0) { return 0.0; }
    let d0 = c.w;
    let l0 = load_gb(p + vec2<i32>(-o, 0));
    let r0 = load_gb(p + vec2<i32>(o, 0));
    let t0 = load_gb(p + vec2<i32>(0, -o));
    let b0 = load_gb(p + vec2<i32>(0, o));
    // A neighbour that is inkless contributes the centre's own depth and
    // normal, so it neither makes a silhouette nor a crease.
    let l = select(l0, c, l0.x < 0.0);
    let r = select(r0, c, r0.x < 0.0);
    let t = select(t0, c, t0.x < 0.0);
    let b = select(b0, c, b0.x < 0.0);
    let nearest = min(min(l.w, r.w), min(t.w, b.w));
    let near_side = select(0.0, 1.0, d0 <= nearest + 0.03 * d0 + 0.05);
    let lap = abs(l.w + r.w - 2.0 * d0) + abs(t.w + b.w - 2.0 * d0);
    let depth_edge = smoothstep(0.6, 1.0, lap / (0.05 * d0 + 0.10)) * near_side;

    let n0 = c.xyz * 2.0 - 1.0;
    let crease = (1.0 - dot(n0, l.xyz * 2.0 - 1.0)) + (1.0 - dot(n0, r.xyz * 2.0 - 1.0))
               + (1.0 - dot(n0, t.xyz * 2.0 - 1.0)) + (1.0 - dot(n0, b.xyz * 2.0 - 1.0));
    // Far away, neighbouring pixels are metres apart and their normals differ
    // by noise alone, so creases fade out with distance.
    let normal_edge = smoothstep(0.14, 0.32, crease) * (1.0 - smoothstep(40.0, 110.0, d0));
    return max(depth_edge, normal_edge);
}

@fragment
fn fs_ink(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let home = vec2<i32>(floor(frag.xy));
    let src = textureLoad(color_tex, home, 0);
    var col = src.rgb;

    if (u.flags.w > 0.5) {
        // Hand-drawn feel: the lines are looked up a pixel or two off, by a
        // slowly varying amount, so they waver. A second noise decides where
        // the pen presses harder (a 2 px stroke instead of 1 px).
        let wob = (vec2<f32>(vnoise2(frag.xy * 0.045), vnoise2(frag.xy * 0.045 + 31.7)) - 0.5) * 2.6;
        let p = vec2<i32>(floor(frag.xy + wob));
        let o = max(1, i32(round(u.size.y / 1000.0)));
        let press = smoothstep(0.35, 0.65, vnoise2(frag.xy * 0.02 + 7.0));
        var line = edge(p, o);
        // The wide stroke costs a second round of G-buffer reads, so only pay for
        // it where the pen presses hard (about a third of the screen).
        if (press > 0.02) {
            line = max(line, edge(p, 2 * o) * press * 0.9);
        }
        col = mix(col, vec3<f32>(0.05, 0.035, 0.07), clamp(line, 0.0, 1.0) * 0.92);
    }
    // Carry the emissive mask forward so bloom can run on the inked picture.
    return vec4<f32>(col, src.a);
}

// Filmic shoulder: keep the painting's midtones, roll hot effects off to white.
fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(1.0) - exp(-c * 1.25);
}

// The glow source at binding 1 is the inked picture for the downsample pass and
// a bloom level for the blur passes; all three are fullscreen and ratio-2.
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;
@group(0) @binding(5) var bloom_tex: texture_2d<f32>;

// First bloom step: halve the ink picture, keeping only the light it emits (the
// alpha mask), with a 3x3 tent. Half resolution is plenty for a soft halo and
// makes the blur below cheap.
@fragment
fn fs_bloom_down(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = frag.xy * 2.0 / u.size.xy;
    let px = 1.0 / u.size.xy;
    let w = vec3<f32>(0.25, 0.125, 0.0625);
    var c = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let s = textureSampleLevel(src_tex, src_smp, uv + vec2<f32>(f32(x), f32(y)) * px, 0.0);
            let weight = select(w.y, w.z, x != 0 && y != 0);
            c += s.rgb * min(s.a, 1.0) * select(weight, w.x, x == 0 && y == 0);
        }
    }
    return vec4<f32>(c, 0.0);
}

// Two separable gaussian blurs: wide enough to spill a fireball's light several
// metres across the rock, smooth enough that a bright ring does not turn into a
// stack of ghost rings (which a one-pass tap ring does).
fn blur(src_uv: vec2<f32>, axis: vec2<f32>) -> vec3<f32> {
    let half = u.size.xy * 0.5;
    let d = axis / half;
    var c = textureSampleLevel(src_tex, src_smp, src_uv, 0.0).rgb * 0.227027;
    c += textureSampleLevel(src_tex, src_smp, src_uv + d * 1.3846, 0.0).rgb * 0.316216;
    c += textureSampleLevel(src_tex, src_smp, src_uv - d * 1.3846, 0.0).rgb * 0.316216;
    c += textureSampleLevel(src_tex, src_smp, src_uv + d * 3.2308, 0.0).rgb * 0.070270;
    c += textureSampleLevel(src_tex, src_smp, src_uv - d * 3.2308, 0.0).rgb * 0.070270;
    return c;
}

@fragment
fn fs_blur_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(frag.xy / (u.size.xy * 0.5), vec2<f32>(1.0, 0.0)), 0.0);
}

@fragment
fn fs_blur_v(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(frag.xy / (u.size.xy * 0.5), vec2<f32>(0.0, 1.0)), 0.0);
}

@fragment
fn fs_present(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = frag.xy / u.size.zw;
    var col = textureSampleLevel(ink_tex, ink_smp, uv, 0.0).rgb;

    // Bloom: the blurred light (fire, plasma, flashes) added in linear light,
    // then tone-mapped. Only emissive pixels contributed, so the sky and pale
    // rock stay clean.
    col += textureSampleLevel(bloom_tex, ink_smp, uv, 0.0).rgb * 0.85;

    // Tone-map the HDR picture. Hot cores roll off to white, which is what
    // makes a fireball look like it is emitting light instead of being orange.
    col = tonemap(col);

    // Impact flash: a white frame with a slight lens split. The two extra
    // texture taps only run while a flash is alive (a blink), so the steady
    // cost is one branch. The centre is hotter than the edges, which keeps the
    // picture readable during the white-out instead of turning it into a slab.
    let flash = clamp(u.fx.x, 0.0, 1.0);
    if (flash > 0.002) {
        let d = uv - vec2<f32>(0.5);
        let ca = flash * 0.010;
        let r = textureSampleLevel(ink_tex, ink_smp, uv + d * ca, 0.0).r;
        let b = textureSampleLevel(ink_tex, ink_smp, uv - d * ca, 0.0).b;
        let split = tonemap(vec3<f32>(r, col.g, b));
        col = vec3<f32>(split.r, col.g, split.b);
        let core = 1.0 - 0.40 * smoothstep(0.0, 0.7, dot(d, d) * 2.0);
        col = mix(col, vec3<f32>(1.0, 0.97, 0.90), flash * core * 0.60);
    }

    // Paper grain and a soft vignette tie the frame together.
    col *= 1.0 + (hash21(frag.xy) - 0.5) * 0.035;
    let c = uv - 0.5;
    col *= 1.0 - 0.28 * dot(c, c) * 2.0;

    // Crosshair: a ring with a dark edge so it reads on any background.
    let rr = length(frag.xy - u.size.zw * 0.5);
    if (rr < 4.2 && rr > 3.0) { col = vec3<f32>(0.05, 0.035, 0.07); }
    if (rr <= 3.0 && rr > 1.8) { col = vec3<f32>(1.0, 0.97, 0.88); }
    return vec4<f32>(col, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{MAX_PRIMS, TILES_X, TILES_Y, TILE_WORDS};

    // The WGSL has these numbers pasted in from the Rust side; a mismatch would
    // silently read the wrong tile or overrun the uniform. Test the resolved
    // strings, which is what the GPU actually compiles.
    #[test]
    fn shader_constants_match_the_renderer() {
        let groups = TILES_X * TILES_Y / 4;
        let u = uniforms();
        assert!(u.contains(&format!("array<Prim, {MAX_PRIMS}>")));
        assert!(!u.contains("struct Stain"), "stains are decals now, not a march uniform");
        assert!(u.contains(&format!("array<vec4<u32>, {}>", TILE_WORDS * groups)));
        assert!(!u.contains('$'), "every uniform placeholder is filled in");
        let m = march(glam::IVec3::splat(2));
        assert!(m.contains(&format!("vec2<f32>({TILES_X}.0, {TILES_Y}.0)")));
        assert!(m.contains(&format!("vec2<u32>({}u, {}u)", TILES_X - 1, TILES_Y - 1)));
        assert!(m.contains(&format!("tile.y * {TILES_X}u")));
        assert!(m.contains(&format!("w < {}u", TILE_WORDS)));
        assert!(m.contains(&format!("w * {}u", groups)));
        assert!(MAX_PRIMS % 32 == 0, "every word must be full");
        let s = sprites(glam::IVec3::splat(2));
        assert!(!s.contains('$'), "every sprite placeholder is filled in");
        assert!(s.contains("texture_3d<f32>"), "blood can sample the terrain");
    }

    // The whole point of the shared chunk: the studio sheet and the game compile
    // the same body, not two copies that can drift. Also checks that the hand
    // placeholders were pasted from the Rust constants before the GPU sees them.
    #[test]
    fn the_sheet_and_the_game_share_one_sculpt() {
        let body = sculpt();
        assert!(!body.contains('$'), "every placeholder is filled in");
        assert!(body.contains(&vec3(crate::worms::SDF_HAND_R)));
        assert!(body.contains(&vec3(crate::worms::SDF_HAND_L)));

        let game = march(glam::IVec3::splat(2));
        let sheet = crate::worm_preview::shader();
        assert_eq!(game.matches(&body).count(), 1, "the game includes the sculpt once");
        assert_eq!(sheet.matches(&body).count(), 1, "the sheet includes it once");
        assert!(!sheet.contains("$SCULPT"), "the sheet placeholder is filled in");
    }
}
