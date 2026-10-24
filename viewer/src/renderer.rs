// All the wgpu code: the terrain as a 3D texture, the two passes, and the
// offscreen target used by `--shot` and `--bench`.
//
// How the terrain gets to the GPU (the central idea of this viewer):
//   * `sdf::Grid` stays the one source of truth on the CPU (collision, edits).
//   * The GPU holds a COPY in a 3D texture, one signed byte per voxel.
//   * Pattern: Dirty Flag. After an edit we re-upload only the changed 16^3
//     chunks (4 KB each). Nothing is ever re-meshed, so an explosion costs the
//     edit itself plus a handful of tiny texture writes.

use crate::camera::Camera;
use crate::shaders;
use bytemuck::Zeroable;
use glam::{IVec3, Vec3};
use sdf::{Grid, CHUNK, CHUNK_VOL};
use std::time::Instant;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const GBUF_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
// Towards the sun. It shines over the spawn camera's shoulder, so the first
// view is lit; the far mountains show their shaded side.
pub const SUN: Vec3 = Vec3::new(0.5, 0.72, -0.45);

pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// True if the GPU can timestamp passes (see `GpuTimer`).
    pub timestamps: bool,
}

impl Gpu {
    /// `surface` is None for the headless modes.
    pub fn new(instance: &wgpu::Instance, surface: Option<&wgpu::Surface>) -> Gpu {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no GPU adapter found");
        let info = adapter.get_info();
        println!("GPU: {} ({:?})", info.name, info.backend);
        // Optional: timestamp queries let us read how long each pass took on the
        // GPU. Without them the viewer still works, it just cannot show GPU times.
        let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("viewer"),
            required_features: if timestamps { wgpu::Features::TIMESTAMP_QUERY } else { wgpu::Features::empty() },
            // The 3D texture is 512 x 256 x 512, well inside the default limit of 2048.
            ..Default::default()
        }))
        .expect("could not open the GPU");
        Gpu { adapter, device, queue, timestamps }
    }
}

/// Everything that changes per frame. The terrain is not in here: it lives in
/// the textures and is updated by `Renderer::sync`.
pub struct FrameParams<'a> {
    pub camera: &'a Camera,
    /// Worms, guns, ropes and effects (at most `MAX_CAPSULES`, the rest are dropped).
    pub capsules: &'a [DrawPrim],
    /// Brush centre and radius, drawn as a tint plus a rim on the terrain.
    pub brush: Option<(Vec3, f32)>,
    pub time: f32,
    pub shadows: bool,
    pub ao: bool,
    pub outlines: bool,
}

/// How many primitives the shader's uniform block holds (`array<Prim, 256>`).
/// Raised above 128 so four worms can each carry an oversized, multi-part weapon
/// with room left over for projectiles and effects.
pub const MAX_CAPSULES: usize = 256;

/// One primitive the raymarcher intersects in closed form (no sphere tracing),
/// so each stays a handful of multiplications per pixel. Worms are mostly
/// capsules; a gun is boxes and cylinders, because flat faces and hard edges are
/// what stop a character reading as a soft, AI-generated blob.
#[derive(Clone, Copy)]
pub enum DrawPrim {
    /// A line segment swept by a sphere (rounded ends).
    Capsule { a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool, glow: bool, inkless: bool },
    /// A line segment swept by a disc (flat ends), for a crisp gun barrel.
    Cylinder { a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool, glow: bool, inkless: bool },
    /// A hard-edged box, long axis along `forward`, its "up" taken from world up.
    Cuboid { centre: Vec3, half: Vec3, forward: Vec3, colour: Vec3, shadow: bool, glow: bool, inkless: bool },
}

impl DrawPrim {
    pub fn capsule(a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool) -> DrawPrim {
        DrawPrim::Capsule { a, b, radius, colour, shadow, glow: false, inkless: false }
    }
    pub fn cylinder(a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool) -> DrawPrim {
        DrawPrim::Cylinder { a, b, radius, colour, shadow, glow: false, inkless: false }
    }
    pub fn cuboid(centre: Vec3, half: Vec3, forward: Vec3, colour: Vec3, shadow: bool) -> DrawPrim {
        DrawPrim::Cuboid { centre, half, forward, colour, shadow, glow: false, inkless: false }
    }

    /// Make it self-lit: the shader skips shading and draws the colour flat and
    /// bright, so a muzzle, coil or hot core reads as a light source instead of
    /// painted plastic.
    pub fn glowing(self) -> DrawPrim {
        match self {
            DrawPrim::Capsule { a, b, radius, colour, shadow, inkless, .. } => {
                DrawPrim::Capsule { a, b, radius, colour, shadow, glow: true, inkless }
            }
            DrawPrim::Cylinder { a, b, radius, colour, shadow, inkless, .. } => {
                DrawPrim::Cylinder { a, b, radius, colour, shadow, glow: true, inkless }
            }
            DrawPrim::Cuboid { centre, half, forward, colour, shadow, inkless, .. } => {
                DrawPrim::Cuboid { centre, half, forward, colour, shadow, glow: true, inkless }
            }
        }
    }

    /// Draw it without an ink outline. Screen-space aids and soft smoke should
    /// not get the same black rim as a solid object: at a distance the outline
    /// is wider than the shape, and a glowing dot turns into a dark pebble.
    pub fn inkless(self) -> DrawPrim {
        match self {
            DrawPrim::Capsule { a, b, radius, colour, shadow, glow, .. } => {
                DrawPrim::Capsule { a, b, radius, colour, shadow, glow, inkless: true }
            }
            DrawPrim::Cylinder { a, b, radius, colour, shadow, glow, .. } => {
                DrawPrim::Cylinder { a, b, radius, colour, shadow, glow, inkless: true }
            }
            DrawPrim::Cuboid { centre, half, forward, colour, shadow, glow, .. } => {
                DrawPrim::Cuboid { centre, half, forward, colour, shadow, glow, inkless: true }
            }
        }
    }

    /// Does it cast a shadow on the terrain? (Bodies do; eyes and ropes are too
    /// small to matter, and every caster costs every terrain pixel a test.)
    pub fn shadow(&self) -> bool {
        match self {
            DrawPrim::Capsule { shadow, .. }
            | DrawPrim::Cylinder { shadow, .. }
            | DrawPrim::Cuboid { shadow, .. } => *shadow,
        }
    }

    fn glow(&self) -> bool {
        match self {
            DrawPrim::Capsule { glow, .. }
            | DrawPrim::Cylinder { glow, .. }
            | DrawPrim::Cuboid { glow, .. } => *glow,
        }
    }

    fn is_inkless(&self) -> bool {
        match self {
            DrawPrim::Capsule { inkless, .. }
            | DrawPrim::Cylinder { inkless, .. }
            | DrawPrim::Cuboid { inkless, .. } => *inkless,
        }
    }

    /// A conservative bounding sphere (centre, radius), used by the tile mask and
    /// the shader's cheap reject before the full intersection.
    fn bounds(&self) -> (Vec3, f32) {
        match self {
            DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => {
                ((*a + *b) * 0.5, radius + (*b - *a).length() * 0.5)
            }
            DrawPrim::Cuboid { centre, half, .. } => (*centre, half.length()),
        }
    }
}

/// The primitive kind stored in the shader's `col.w`.
const KIND_CAPSULE: f32 = 0.0;
const KIND_CYLINDER: f32 = 1.0;
const KIND_CUBOID: f32 = 2.0;

/// The screen is cut into 32 x 18 tiles, and every tile gets a bitmask of the
/// capsules that can be seen in it. Why: looping over all capsules for every
/// pixel cost ~6 ms for a handful of worms on this GPU, even with a trivial
/// loop body (measured: docs/PERFORMANCE.md). With the mask, a pixel far from
/// every worm reads one number, sees zero, and skips all of it.
pub const TILES_X: usize = 32;
pub const TILES_Y: usize = 18;

// Must match `struct Prim` in shaders.rs: four vec4s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuPrim {
    a: [f32; 4],      // capsule/cylinder: start.xyz + radius; cuboid: centre.xyz
    b: [f32; 4],      // capsule/cylinder: end.xyz; cuboid: half extents. w = bound radius
    c: [f32; 4],      // cuboid: forward axis.xyz; capsule/cylinder: unused
    colour: [f32; 4], // rgb + kind (0 capsule, 1 cylinder, 2 cuboid)
}

// Must match `struct U` in shaders.rs: vec4s all the way, so no padding questions.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cam_pos: [f32; 4],
    cam_right: [f32; 4],
    cam_up: [f32; 4],
    cam_fwd: [f32; 4],
    sun: [f32; 4],
    brush: [f32; 4],
    flags: [f32; 4],
    size: [f32; 4],
    counts: [f32; 4], // x: number of primitives in use, y: shadow casters (the first y)
    capsules: [GpuPrim; MAX_CAPSULES],
    tiles: [[u32; 4]; TILE_WORDS * TILES_X * TILES_Y / 4],
}

/// The number of 4-tile groups in one word of the tile masks.
const TILE_GROUPS: usize = TILES_X * TILES_Y / 4;

/// How many 32-bit words a tile mask needs to cover every capsule bit. This is
/// the one number that grows with `MAX_CAPSULES`: 64 capsules fit in two words,
/// 128 in four. `tile_masks` packs that many blocks and the shader reads them.
pub const TILE_WORDS: usize = MAX_CAPSULES / 32;

/// For every screen tile, which capsules (bit i = capsule i) might be seen in it.
/// Conservative: a tile may be marked without a capsule in it, never the reverse.
/// `TILE_WORDS` 32-bit words per tile are packed as that many blocks, so the
/// shader can pick the block with the capsule index.
fn tile_masks(cam: &Camera, aspect: f32, prims: &[&DrawPrim]) -> [[u32; 4]; TILE_WORDS * TILE_GROUPS] {
    let mut words = [[0u32; TILE_WORDS]; TILES_X * TILES_Y];
    let (right, up) = cam.right_up();
    let fwd = cam.forward();
    let (hy, hx) = ((cam.fov_y * 0.5).tan(), (cam.fov_y * 0.5).tan() * aspect);
    for (i, c) in prims.iter().enumerate() {
        // A sphere around the whole primitive, then the sphere's rectangle on screen.
        let (mid, r) = c.bounds();
        let rel = mid - cam.pos;
        let depth = rel.dot(fwd);
        if depth + r < 0.0 {
            continue; // entirely behind the camera
        }
        let (mut x0, mut x1, mut y0, mut y1) = (0, TILES_X - 1, 0, TILES_Y - 1);
        if depth - r > 0.1 {
            // (nx, ny): the centre in screen coordinates, -1..1, y up.
            let (nx, ny) = (rel.dot(right) / (depth * hx), rel.dot(up) / (depth * hy));
            // Off to the side a sphere looks stretched; this is generous (the true
            // factor is its square root) and costs only a few extra tiles.
            let stretch = 1.0 + (nx * hx) * (nx * hx) + (ny * hy) * (ny * hy);
            let (rx, ry) = (r / (depth * hx) * stretch, r / (depth * hy) * stretch);
            // To tile coordinates (y down), with one tile of margin.
            let tx = |n: f32| (n * 0.5 + 0.5) * TILES_X as f32;
            let ty = |n: f32| (0.5 - n * 0.5) * TILES_Y as f32;
            let tile = |v: f32, count: usize| (v.floor() as i32).clamp(0, count as i32 - 1) as usize;
            x0 = tile(tx(nx - rx) - 1.0, TILES_X);
            x1 = tile(tx(nx + rx) + 1.0, TILES_X);
            y0 = tile(ty(ny + ry) - 1.0, TILES_Y);
            y1 = tile(ty(ny - ry) + 1.0, TILES_Y);
        }
        let (word, bit) = (i / 32, i % 32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                words[y * TILES_X + x][word] |= 1 << bit;
            }
        }
    }
    let mut packed = [[0u32; 4]; TILE_WORDS * TILE_GROUPS];
    for (k, w) in words.iter().enumerate() {
        for word in 0..TILE_WORDS {
            packed[word * TILE_GROUPS + k / 4][k % 4] = w[word];
        }
    }
    packed
}

/// Pack a `DrawPrim` into the GPU's uniform layout (see `struct Prim` in
/// shaders.rs). The kind is stashed in the colour's alpha channel and the draw
/// flags in `c.w` (the only float every kind leaves free): bit 0 = self-lit,
/// bit 1 = no ink outline.
fn pack(c: &DrawPrim) -> GpuPrim {
    let mut flags = 0.0;
    if c.glow() {
        flags += 1.0;
    }
    if c.is_inkless() {
        flags += 2.0;
    }
    let (a, b, radius, colour, kind) = match *c {
        DrawPrim::Capsule { a, b, radius, colour, .. } => (a, b, radius, colour, KIND_CAPSULE),
        DrawPrim::Cylinder { a, b, radius, colour, .. } => (a, b, radius, colour, KIND_CYLINDER),
        DrawPrim::Cuboid { centre, half, forward, colour, .. } => {
            return GpuPrim {
                a: centre.extend(0.0).to_array(),
                b: half.extend(half.length()).to_array(),
                c: forward.normalize_or_zero().extend(flags).to_array(),
                colour: colour.extend(KIND_CUBOID).to_array(),
            };
        }
    };
    GpuPrim {
        a: a.extend(radius).to_array(),
        b: b.extend(radius + (b - a).length() * 0.5).to_array(),
        c: [0.0, 0.0, 0.0, flags],
        colour: colour.extend(kind).to_array(),
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    world_chunks: IVec3,
    sdf_tex: wgpu::Texture,
    coarse_tex: wgpu::Texture,
    uniforms: wgpu::Buffer,
    march_pipeline: wgpu::RenderPipeline,
    march_bind_group: wgpu::BindGroup,
    ink_pipeline: wgpu::RenderPipeline,
    ink_layout: wgpu::BindGroupLayout,
    present_pipeline: wgpu::RenderPipeline,
    present_layout: wgpu::BindGroupLayout,
    upscale_sampler: wgpu::Sampler,
    targets: Targets,
    screen: (u32, u32),
    gbuf: (u32, u32),
    scale: f32,
    scratch: Box<[i8; CHUNK_VOL]>,
    /// Staging area for batched uploads, reused so edits allocate nothing.
    region: Vec<i8>,
    /// Which chunks held a surface at the last empty-space map upload.
    surface_cache: Vec<u8>,
    timer: Option<GpuTimer>,
}

impl Renderer {
    /// Builds the pipelines and uploads the whole terrain once.
    pub fn new(gpu: &Gpu, target_format: wgpu::TextureFormat, screen: (u32, u32), scale: f32, grid: &mut Grid) -> Renderer {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let world_chunks = grid.size_chunks();
        let dims = world_chunks * CHUNK;

        let sdf_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terrain sdf"),
            size: wgpu::Extent3d { width: dims.x as u32, height: dims.y as u32, depth_or_array_layers: dims.z as u32 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            // Snorm: the byte -127..127 is read back as -1..1, and the sampler
            // may interpolate it (trilinear), which is what makes the surface smooth.
            format: wgpu::TextureFormat::R8Snorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let coarse_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("empty-space map"),
            size: wgpu::Extent3d {
                width: world_chunks.x as u32,
                height: world_chunks.y as u32,
                depth_or_array_layers: world_chunks.z as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terrain sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default() // clamp to edge
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- pass 1: march ---------------------------------------------------
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let uniform_ty = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let tex3d = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        };
        let march_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("march layout"),
            entries: &[
                entry(0, uniform_ty),
                entry(1, tex3d(wgpu::TextureSampleType::Float { filterable: true })),
                entry(2, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                entry(3, tex3d(wgpu::TextureSampleType::Uint)),
            ],
        });
        let sdf_view = sdf_tex.create_view(&Default::default());
        let coarse_view = coarse_tex.create_view(&Default::default());
        let march_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("march bind group"),
            layout: &march_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&sdf_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&coarse_view) },
            ],
        });
        let march_pipeline = fullscreen_pipeline(
            &device,
            "march",
            &shaders::march(world_chunks),
            "fs",
            &march_layout,
            &[COLOR_FORMAT, GBUF_FORMAT],
        );

        // --- pass 2: ink outlines, at G-buffer resolution -----------------------
        let tex2d = |filterable| wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        // The G-buffer (depth, normals) is read texel by texel: you cannot
        // average a depth edge.
        let ink_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ink layout"),
            entries: &[entry(0, uniform_ty), entry(1, tex2d(false)), entry(2, tex2d(false))],
        });
        let post_wgsl = shaders::post();
        let ink_pipeline = fullscreen_pipeline(&device, "ink", &post_wgsl, "fs_ink", &ink_layout, &[COLOR_FORMAT]);

        // --- pass 3: upscale to the screen --------------------------------------
        // The ink image is sampled with a bilinear filter, so rendering below
        // native resolution looks soft instead of blocky.
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present layout"),
            entries: &[
                entry(0, uniform_ty),
                entry(3, tex2d(true)),
                entry(4, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let present_pipeline =
            fullscreen_pipeline(&device, "present", &post_wgsl, "fs_present", &present_layout, &[target_format]);
        let upscale_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("upscale sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let gbuf = scaled(screen, scale);
        let targets = make_targets(&device, gbuf, &ink_layout, &present_layout, &uniforms, &upscale_sampler);

        let mut r = Renderer {
            device,
            queue,
            world_chunks,
            sdf_tex,
            coarse_tex,
            uniforms,
            march_pipeline,
            march_bind_group,
            ink_pipeline,
            ink_layout,
            present_pipeline,
            present_layout,
            upscale_sampler,
            targets,
            screen,
            gbuf,
            scale,
            scratch: Box::new([0; CHUNK_VOL]),
            region: Vec::new(),
            surface_cache: Vec::new(),
            timer: if gpu.timestamps { Some(GpuTimer::new(&gpu.device, gpu.queue.get_timestamp_period())) } else { None },
        };
        r.upload_all(grid);
        r
    }

    pub fn resize(&mut self, screen: (u32, u32)) {
        self.screen = screen;
        self.rebuild_gbuffers();
    }

    /// Render-resolution scale (1.0 = native). The march pass is the expensive
    /// one and its cost is per pixel, so this is the quickest speed knob.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
        self.rebuild_gbuffers();
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    fn rebuild_gbuffers(&mut self) {
        self.gbuf = scaled(self.screen, self.scale);
        self.targets = make_targets(
            &self.device,
            self.gbuf,
            &self.ink_layout,
            &self.present_layout,
            &self.uniforms,
            &self.upscale_sampler,
        );
    }

    /// Whole terrain, one z-layer of chunks at a time so the staging memory
    /// stays at a few MB instead of the full 67 MB.
    fn upload_all(&mut self, grid: &mut Grid) {
        let t = Instant::now();
        let n = self.world_chunks;
        for cz in 0..n.z {
            self.upload_region(grid, IVec3::new(0, 0, cz), IVec3::new(n.x - 1, n.y - 1, cz));
        }
        grid.take_dirty(); // everything is on the GPU now
        self.upload_coarse(grid);
        let dims = n * CHUNK;
        println!(
            "terrain uploaded: {} MB in {:.0} ms",
            dims.x as i64 * dims.y as i64 * dims.z as i64 / (1 << 20),
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Copy the box of chunks `lo..=hi` to the texture in ONE write.
    ///
    /// Why a box and not one write per chunk: a crater dirties ~20 chunks, and
    /// 20 separate `write_texture` calls cost ~1 ms each in the driver (measured:
    /// 23 ms per crater), far more than the bytes themselves. One call over the
    /// bounding box moves a few more bytes and costs about one call.
    fn upload_region(&mut self, grid: &Grid, lo: IVec3, hi: IVec3) {
        let n = hi - lo + IVec3::ONE;
        let vox = n * CHUNK;
        self.region.resize((vox.x * vox.y * vox.z) as usize, 0);
        for cz in 0..n.z {
            for cy in 0..n.y {
                for cx in 0..n.x {
                    grid.copy_chunk(lo + IVec3::new(cx, cy, cz), &mut self.scratch);
                    // Scatter the chunk's 16 x 16 rows of 16 voxels into the region.
                    for lz in 0..CHUNK {
                        for ly in 0..CHUNK {
                            let src = ((lz * CHUNK + ly) * CHUNK) as usize;
                            let dst = (((cz * CHUNK + lz) * vox.y + cy * CHUNK + ly) * vox.x + cx * CHUNK) as usize;
                            self.region[dst..dst + CHUNK as usize]
                                .copy_from_slice(&self.scratch[src..src + CHUNK as usize]);
                        }
                    }
                }
            }
        }
        let o = lo * CHUNK;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.sdf_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: o.x as u32, y: o.y as u32, z: o.z as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.region),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(vox.x as u32),
                rows_per_image: Some(vox.y as u32),
            },
            wgpu::Extent3d { width: vox.x as u32, height: vox.y as u32, depth_or_array_layers: vox.z as u32 },
        );
    }

    /// Push the chunks edited since the last call to the GPU. Returns how many.
    pub fn sync(&mut self, grid: &mut Grid) -> usize {
        let dirty = grid.take_dirty();
        if dirty.is_empty() {
            return 0;
        }
        let lo = dirty.iter().fold(IVec3::MAX, |a, c| a.min(*c));
        let hi = dirty.iter().fold(IVec3::MIN, |a, c| a.max(*c));
        let size = hi - lo + IVec3::ONE;
        // One box if the edits sit together (a crater, a beam sweep). If they are
        // scattered (two players digging far apart) a box would drag in the whole
        // world between them, so fall back to one write per chunk.
        if size.x * size.y * size.z <= 4 * dirty.len() as i32 + 8 {
            self.upload_region(grid, lo, hi);
        } else {
            for c in &dirty {
                self.upload_region(grid, *c, *c);
            }
        }
        // An edit can turn an empty chunk into a surface chunk, so the skipping
        // map has to follow. It is only 16 KB.
        self.upload_coarse(grid);
        dirty.len()
    }

    fn upload_coarse(&mut self, grid: &Grid) {
        // Digging inside chunks that already hold surface changes nothing here,
        // and that is most edits after the first: skip the rebuild and upload.
        let flags = surface_flags(grid);
        if flags == self.surface_cache {
            return;
        }
        let n = self.world_chunks;
        let map = chessboard_distance(&flags, n);
        self.surface_cache = flags;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.coarse_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &map,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n.x as u32), rows_per_image: Some(n.y as u32) },
            wgpu::Extent3d { width: n.x as u32, height: n.y as u32, depth_or_array_layers: n.z as u32 },
        );
    }

    /// Record both passes. `target` must have the format given to `new`.
    pub fn render(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, f: &FrameParams) {
        let cam = f.camera;
        let (right, up) = cam.right_up();
        let half = (cam.fov_y * 0.5).tan();
        let aspect = self.screen.0 as f32 / self.screen.1 as f32;
        let (brush_pos, brush_r) = f.brush.unwrap_or((Vec3::ZERO, 0.0));
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        // Shadow casters first: the shader's shadow loop runs over just those.
        let ordered: Vec<&DrawPrim> = (f.capsules.iter().filter(|c| c.shadow()))
            .chain(f.capsules.iter().filter(|c| !c.shadow()))
            .take(MAX_CAPSULES)
            .collect();
        let casters = ordered.iter().filter(|c| c.shadow()).count();
        let tiles = tile_masks(cam, aspect, &ordered);
        let mut capsules = [GpuPrim::zeroed(); MAX_CAPSULES];
        let used = ordered.len();
        for (gpu, c) in capsules.iter_mut().zip(&ordered) {
            *gpu = pack(c);
        }
        let u = Uniforms {
            cam_pos: cam.pos.extend(0.0).to_array(),
            cam_right: (right * half * aspect).extend(0.0).to_array(),
            cam_up: (up * half).extend(0.0).to_array(),
            cam_fwd: cam.forward().extend(0.0).to_array(),
            sun: SUN.normalize().extend(0.0).to_array(),
            brush: brush_pos.extend(brush_r).to_array(),
            flags: [f.time, flag(f.shadows), flag(f.ao), flag(f.outlines)],
            size: [self.gbuf.0 as f32, self.gbuf.1 as f32, self.screen.0 as f32, self.screen.1 as f32],
            counts: [used as f32, casters as f32, 0.0, 0.0],
            capsules,
            tiles,
        };
        self.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));

        let slot = self.timer.as_mut().and_then(|t| t.begin(&self.device));
        let timer = self.timer.as_ref();
        // Slots 0 and 1 bracket the march pass; slot 2 is written as the ink pass
        // starts and slot 3 as the present pass ends, so "post" covers both.
        let stamps = |begin: Option<u32>, end: Option<u32>| {
            timer.zip(slot).map(|(t, _)| wgpu::RenderPassTimestampWrites {
                query_set: &t.queries,
                beginning_of_pass_write_index: begin,
                end_of_pass_write_index: end,
            })
        };
        {
            let attach = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("march"),
                color_attachments: &[attach(&self.targets.color), attach(&self.targets.gbuf)],
                depth_stencil_attachment: None,
                timestamp_writes: stamps(Some(0), Some(1)),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.march_pipeline);
            pass.set_bind_group(0, &self.march_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ink"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.ink,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: stamps(Some(2), None),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.ink_pipeline);
            pass.set_bind_group(0, &self.targets.ink_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: stamps(None, Some(3)),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.present_pipeline);
            pass.set_bind_group(0, &self.targets.present_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        if let (Some(t), Some(slot)) = (self.timer.as_ref(), slot) {
            t.resolve(encoder, slot);
        }
    }

    /// Call right after `queue.submit` for the frame just rendered.
    pub fn end_frame(&mut self) {
        if let Some(t) = self.timer.as_mut() {
            t.request_readback();
        }
    }

    /// Latest GPU time of (march pass, post pass) in ms, if the GPU supports
    /// timestamps. Results arrive a frame or two late, because the GPU has to
    /// finish the frame before the numbers can be copied back.
    pub fn gpu_ms(&mut self) -> Option<(f32, f32)> {
        self.timer.as_mut().and_then(|t| t.latest(&self.device))
    }
}

/// How long each pass took on the GPU, measured by the GPU itself.
///
/// Wall-clock timing from the CPU cannot tell GPU time from vsync waits and
/// driver queueing, and on an integrated GPU it is very noisy. Timestamp
/// queries record the GPU's own clock at the start and end of each pass. The
/// result lives in GPU memory, so it is copied into a buffer the CPU can map,
/// which only completes after the frame has finished. A few buffers rotate so
/// the next frame never waits for the previous frame's readback.
struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<TimerSlot>,
    next: usize,
    /// Slot recorded this frame whose readback has not been requested yet.
    recorded: Option<usize>,
    period_ns: f32,
    last: Option<(f32, f32)>,
}

struct TimerSlot {
    buffer: wgpu::Buffer,
    /// 0 idle, 1 copy recorded, 2 mapping requested, 3 mapped and ready to read.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl GpuTimer {
    const SLOTS: usize = 3;
    const BYTES: u64 = 4 * 8; // four u64 timestamps

    fn new(device: &wgpu::Device, period_ns: f32) -> GpuTimer {
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("pass timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 4,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: Self::BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        GpuTimer {
            queries,
            resolve: buffer("timestamp resolve", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            slots: (0..Self::SLOTS)
                .map(|_| TimerSlot {
                    buffer: buffer("timestamp readback", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
                    state: Default::default(),
                })
                .collect(),
            next: 0,
            recorded: None,
            period_ns,
            last: None,
        }
    }

    /// Pick a free readback slot for this frame, or None if all are still busy
    /// (then this frame simply goes unmeasured).
    fn begin(&mut self, device: &wgpu::Device) -> Option<usize> {
        use std::sync::atomic::Ordering::{Acquire, Release};
        self.collect(device);
        let slot = (0..Self::SLOTS).map(|i| (self.next + i) % Self::SLOTS).find(|&i| self.slots[i].state.load(Acquire) == 0)?;
        self.slots[slot].state.store(1, Release);
        self.next = (slot + 1) % Self::SLOTS;
        self.recorded = Some(slot);
        Some(slot)
    }

    fn resolve(&self, encoder: &mut wgpu::CommandEncoder, slot: usize) {
        encoder.resolve_query_set(&self.queries, 0..4, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.slots[slot].buffer, 0, Self::BYTES);
    }

    fn request_readback(&mut self) {
        use std::sync::atomic::Ordering::Release;
        if let Some(slot) = self.recorded.take() {
            let state = self.slots[slot].state.clone();
            state.store(2, Release);
            self.slots[slot].buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                state.store(if r.is_ok() { 3 } else { 0 }, Release);
            });
        }
    }

    /// Read every slot whose mapping has completed, oldest first.
    fn collect(&mut self, device: &wgpu::Device) {
        use std::sync::atomic::Ordering::{Acquire, Release};
        let _ = device.poll(wgpu::PollType::Poll); // lets finished map callbacks run
        for i in 0..Self::SLOTS {
            let slot = (self.next + i) % Self::SLOTS;
            if self.slots[slot].state.load(Acquire) != 3 {
                continue;
            }
            {
                let view = self.slots[slot].buffer.slice(..).get_mapped_range().expect("timestamp range");
                let t: &[u64] = bytemuck::cast_slice(&view);
                let ms = |a: u64, b: u64| b.saturating_sub(a) as f32 * self.period_ns / 1.0e6;
                self.last = Some((ms(t[0], t[1]), ms(t[2], t[3])));
            }
            self.slots[slot].buffer.unmap();
            self.slots[slot].state.store(0, Release);
        }
    }

    fn latest(&mut self, device: &wgpu::Device) -> Option<(f32, f32)> {
        self.collect(device);
        self.last
    }
}

fn scaled(screen: (u32, u32), scale: f32) -> (u32, u32) {
    (((screen.0 as f32 * scale).round() as u32).max(1), ((screen.1 as f32 * scale).round() as u32).max(1))
}

/// The images the passes hand to each other, all at the internal resolution:
/// colour and G-buffer (written by the march), then the inked picture. They are
/// recreated whenever the window or the render scale changes.
struct Targets {
    color: wgpu::TextureView,
    gbuf: wgpu::TextureView,
    ink: wgpu::TextureView,
    ink_bind_group: wgpu::BindGroup,
    present_bind_group: wgpu::BindGroup,
}

fn make_targets(
    device: &wgpu::Device,
    (w, h): (u32, u32),
    ink_layout: &wgpu::BindGroupLayout,
    present_layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
) -> Targets {
    let make = |label, format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    // Colour is stored sRGB-encoded in 8 bits (a toon image has no use for
    // more); normal + distance need float precision, hence 16-bit floats.
    let color = make("g colour", COLOR_FORMAT);
    let gbuf = make("g normal+depth", GBUF_FORMAT);
    let ink = make("inked picture", COLOR_FORMAT);
    let uniform = || wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() };
    let ink_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ink bind group"),
        layout: ink_layout,
        entries: &[
            uniform(),
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&color) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&gbuf) },
        ],
    });
    let present_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("present bind group"),
        layout: present_layout,
        entries: &[
            uniform(),
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&ink) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    });
    Targets { color, gbuf, ink, ink_bind_group, present_bind_group }
}

/// Every pass is "draw one triangle that covers the screen".
fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    wgsl: &str,
    fs_entry: &str,
    bind_layout: &wgpu::BindGroupLayout,
    formats: &[wgpu::TextureFormat],
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bind_layout)],
        immediate_size: 0,
    });
    let targets: Vec<Option<wgpu::ColorTargetState>> = formats
        .iter()
        .map(|&format| Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL }))
        .collect();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(fs_entry),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// The empty-space map: for every chunk, the chessboard distance (in chunks)
/// to the nearest chunk that holds a surface. 0 = holds one, 255 = none at all.
///
/// Pattern: Spatial Partition, used as an acceleration structure. A ray in a
/// chunk with value k knows nothing solid is nearer than (k-1) chunks, so it
/// can leap that far without sampling the terrain.
///
/// Two sweeps with a 3x3x3 neighbourhood give the exact chessboard distance:
/// forward over the 13 neighbours that come earlier in sweep order, then
/// backward over the other 13. The array has a one-chunk border of "infinity"
/// so no neighbour lookup ever needs a bounds check. This runs after every
/// edit, so it has to stay well under a millisecond.
pub fn coarse_map(grid: &Grid) -> Vec<u8> {
    chessboard_distance(&surface_flags(grid), grid.size_chunks())
}

/// 0 for every chunk that holds a surface, 255 for the rest.
fn surface_flags(grid: &Grid) -> Vec<u8> {
    let n = grid.size_chunks();
    let mut flags = Vec::with_capacity((n.x * n.y * n.z) as usize);
    for z in 0..n.z {
        for y in 0..n.y {
            for x in 0..n.x {
                flags.push(if grid.chunk_has_surface(IVec3::new(x, y, z)) { 0 } else { 255 });
            }
        }
    }
    flags
}

fn chessboard_distance(flags: &[u8], n: IVec3) -> Vec<u8> {
    let (sx, sy, sz) = (n.x + 2, n.y + 2, n.z + 2);
    let at = |x: i32, y: i32, z: i32| (((z + 1) * sy + (y + 1)) * sx + (x + 1)) as usize;
    let mut d = vec![255u8; (sx * sy * sz) as usize];
    for z in 0..n.z {
        for y in 0..n.y {
            for x in 0..n.x {
                d[at(x, y, z)] = flags[((z * n.y + y) * n.x + x) as usize];
            }
        }
    }
    // Index offsets of the neighbours visited before / after a cell.
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for dz in -1..=1i32 {
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                let offset = ((dz * sy + dy) * sx + dx) as isize;
                if dz < 0 || (dz == 0 && (dy < 0 || (dy == 0 && dx < 0))) {
                    before.push(offset);
                } else if (dz, dy, dx) != (0, 0, 0) {
                    after.push(offset);
                }
            }
        }
    }
    let relax = |d: &mut [u8], i: usize, neighbours: &[isize]| {
        let mut best = d[i];
        for &o in neighbours {
            best = best.min(d[(i as isize + o) as usize].saturating_add(1));
        }
        d[i] = best;
    };
    for z in 0..n.z {
        for y in 0..n.y {
            for x in 0..n.x {
                relax(&mut d, at(x, y, z), &before);
            }
        }
    }
    for z in (0..n.z).rev() {
        for y in (0..n.y).rev() {
            for x in (0..n.x).rev() {
                relax(&mut d, at(x, y, z), &after);
            }
        }
    }
    // Copy out the interior, without the border.
    let mut out = Vec::with_capacity((n.x * n.y * n.z) as usize);
    for z in 0..n.z {
        for y in 0..n.y {
            let row = at(0, y, z);
            out.extend_from_slice(&d[row..row + n.x as usize]);
        }
    }
    out
}

/// A render target we can read back: for `--shot` (save a PNG) and `--bench`.
pub struct Offscreen {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    size: (u32, u32),
}

impl Offscreen {
    pub const FORMAT: wgpu::TextureFormat = COLOR_FORMAT;

    pub fn new(device: &wgpu::Device, size: (u32, u32)) -> Offscreen {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Offscreen { texture, view, size }
    }

    /// RGBA8 pixels, rows tightly packed (sRGB-encoded, ready for a PNG).
    pub fn read(&self, gpu: &Gpu) -> Vec<u8> {
        let (w, h) = self.size;
        // Buffer rows must be a multiple of 256 bytes.
        let padded = (w * 4).div_ceil(256) * 256;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map failed"));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll failed");
        let data = slice.get_mapped_range().expect("readback range");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdf::{Op, Shape};

    fn caps(a: Vec3, b: Vec3, radius: f32) -> DrawPrim {
        DrawPrim::capsule(a, b, radius, Vec3::ONE, true)
    }

    #[test]
    fn draw_flags_reach_the_shader() {
        let plain = pack(&caps(Vec3::ZERO, Vec3::Y, 0.2));
        assert_eq!(plain.c[3], 0.0, "lit, inked");
        let hot = pack(&caps(Vec3::ZERO, Vec3::Y, 0.2).glowing());
        assert_eq!(hot.c[3], 1.0, "self-lit");
        let aid = pack(&caps(Vec3::ZERO, Vec3::Y, 0.2).glowing().inkless());
        assert_eq!(aid.c[3], 3.0, "self-lit and no outline");
        let smoke = pack(&caps(Vec3::ZERO, Vec3::Y, 0.2).inkless());
        assert_eq!(smoke.c[3], 2.0, "no outline");
    }

    fn bit_set(masks: &[[u32; 4]], tx: usize, ty: usize, bit: usize) -> bool {
        let k = ty * TILES_X + tx;
        let groups = TILES_X * TILES_Y / 4;
        (masks[bit / 32 * groups + k / 4][k % 4] & (1u32 << (bit % 32))) != 0u32
    }

    #[test]
    fn tile_masks_cover_every_visible_point_of_a_capsule() {
        // The culling must be conservative: if any pixel could see the capsule,
        // its tile must say so, or a worm would be cut off along a tile edge.
        let aspect = 16.0 / 9.0;
        let (hy, hx) = ((70f32.to_radians() * 0.5).tan(), (70f32.to_radians() * 0.5).tan() * aspect);
        let capsule = caps(Vec3::new(2.0, -1.0, 12.0), Vec3::new(2.5, 1.0, 14.0), 0.45);
        let (a, b, radius) = (Vec3::new(2.0, -1.0, 12.0), Vec3::new(2.5, 1.0, 14.0), 0.45);
        for yaw in [-1.2f32, -0.5, 0.0, 0.4, 1.3] {
            for pitch in [-0.8f32, 0.0, 0.6] {
                let cam = Camera { pos: Vec3::new(0.3, 0.2, 0.0), yaw, pitch, fov_y: 70f32.to_radians() };
                let masks = tile_masks(&cam, aspect, &[&capsule]);
                let (right, up) = cam.right_up();
                let fwd = cam.forward();
                // Points on and around the capsule's surface.
                for k in 0..=10 {
                    let centre = a.lerp(b, k as f32 / 10.0);
                    for dir in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
                        let rel = centre + dir * radius - cam.pos;
                        let depth = rel.dot(fwd);
                        if depth < 0.2 {
                            continue;
                        }
                        let (nx, ny) = (rel.dot(right) / (depth * hx), rel.dot(up) / (depth * hy));
                        if nx.abs() >= 1.0 || ny.abs() >= 1.0 {
                            continue; // off screen
                        }
                        let tx = ((nx * 0.5 + 0.5) * TILES_X as f32) as usize;
                        let ty = ((0.5 - ny * 0.5) * TILES_Y as f32) as usize;
                        assert!(bit_set(&masks, tx, ty, 0), "yaw {yaw} pitch {pitch}: tile ({tx},{ty}) misses a visible point");
                    }
                }
            }
        }
    }

    #[test]
    fn tile_masks_cover_a_cuboid() {
        // The bounding sphere of a box must be conservative: every corner that
        // faces the camera has to land in a marked tile, or the box is clipped.
        let aspect = 16.0 / 9.0;
        let (hy, hx) = ((70f32.to_radians() * 0.5).tan(), (70f32.to_radians() * 0.5).tan() * aspect);
        let half = Vec3::new(0.3, 0.2, 0.9);
        let cuboid = DrawPrim::cuboid(Vec3::new(0.0, 0.0, 12.0), half, Vec3::Z, Vec3::ONE, true);
        let cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 70f32.to_radians() };
        let masks = tile_masks(&cam, aspect, &[&cuboid]);
        let (right, up) = cam.right_up();
        let fwd = cam.forward();
        for (sx, sy, sz) in [(1.0f32, 1.0f32, 1.0f32), (1.0, 1.0, -1.0), (1.0, -1.0, 1.0), (1.0, -1.0, -1.0), (-1.0, 1.0, 1.0), (-1.0, 1.0, -1.0), (-1.0, -1.0, 1.0), (-1.0, -1.0, -1.0)] {
            let p = Vec3::new(half.x * sx, half.y * sy, 12.0 + half.z * sz);
            let rel = p - cam.pos;
            let depth = rel.dot(fwd);
            if depth < 0.2 {
                continue;
            }
            let (nx, ny) = (rel.dot(right) / (depth * hx), rel.dot(up) / (depth * hy));
            if nx.abs() >= 1.0 || ny.abs() >= 1.0 {
                continue;
            }
            let tx = ((nx * 0.5 + 0.5) * TILES_X as f32) as usize;
            let ty = ((0.5 - ny * 0.5) * TILES_Y as f32) as usize;
            assert!(bit_set(&masks, tx, ty, 0), "corner ({sx},{sy},{sz}) tile ({tx},{ty}) misses");
        }
    }

    #[test]
    fn tile_masks_leave_the_rest_of_the_screen_alone() {
        let cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 70f32.to_radians() };
        let far = caps(Vec3::new(0.0, 0.0, 30.0), Vec3::new(0.0, 1.0, 30.0), 0.45);
        let behind = caps(Vec3::new(0.0, 0.0, -30.0), Vec3::new(0.0, 1.0, -30.0), 0.45);
        let masks = tile_masks(&cam, 16.0 / 9.0, &[&far, &behind]);
        let set = |bit: usize| {
            (0..TILES_X * TILES_Y)
                .filter(|&k| bit_set(&masks, k % TILES_X, k / TILES_X, bit))
                .count()
        };
        assert!(set(0) >= 1 && set(0) <= 25, "a distant worm covers a few of the 576 tiles: {}", set(0));
        assert!(bit_set(&masks, TILES_X / 2, TILES_Y / 2, 0) || bit_set(&masks, TILES_X / 2 - 1, TILES_Y / 2, 0));
        assert_eq!(set(1), 0, "behind the camera is nowhere");
    }

    #[test]
    fn tile_masks_cover_capsules_in_every_word() {
        // Bits 39 and 100 land in the second and fourth words: a capsule must
        // still be visible there, or everything past word zero disappears.
        let capsule = caps(Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.0, 1.0, 10.0), 0.5);
        let refs: Vec<&DrawPrim> = (0..MAX_CAPSULES).map(|_| &capsule).collect();
        let cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 70f32.to_radians() };
        let masks = tile_masks(&cam, 16.0 / 9.0, &refs);
        assert!(bit_set(&masks, TILES_X / 2, TILES_Y / 2, 39));
        assert!(bit_set(&masks, TILES_X / 2, TILES_Y / 2, 100));
    }

    #[test]
    fn coarse_map_counts_chunks_to_the_nearest_surface() {
        // 8x1x8 chunks of rock, a small crater in chunk (1,0,1): only that
        // chunk (and any it spills into) holds a surface.
        let mut g = Grid::new(IVec3::new(8, 1, 8));
        let chunk_m = CHUNK as f32 * sdf::VOX;
        g.apply(Shape::Sphere { center: Vec3::new(1.5, 0.5, 1.5) * chunk_m, radius: 0.5 }, Op::Carve);
        let map = coarse_map(&g);
        let at = |x: i32, z: i32| map[(z * 8 + x) as usize];
        assert_eq!(at(1, 1), 0);
        assert_eq!(at(2, 1), 1);
        assert_eq!(at(3, 3), 2, "chessboard distance, so diagonals count as one step per chunk");
        assert_eq!(at(7, 1), 6);
    }

    #[test]
    fn coarse_map_is_conservative_everywhere() {
        // The ray leaps (k-1) chunks of clear space. Verify against brute force
        // that no chunk claims more clearance than really exists.
        let mut g = Grid::new(IVec3::new(6, 3, 6));
        let chunk_m = CHUNK as f32 * sdf::VOX;
        g.apply(Shape::Sphere { center: Vec3::new(1.0, 1.0, 1.0) * chunk_m, radius: 0.9 }, Op::Carve);
        g.apply(Shape::Sphere { center: Vec3::new(4.5, 2.0, 4.0) * chunk_m, radius: 1.3 }, Op::Carve);
        let map = coarse_map(&g);
        let n = g.size_chunks();
        for z in 0..n.z {
            for y in 0..n.y {
                for x in 0..n.x {
                    let k = map[((z * n.y + y) * n.x + x) as usize] as i32;
                    for sz in 0..n.z {
                        for sy in 0..n.y {
                            for sx in 0..n.x {
                                if g.chunk_has_surface(IVec3::new(sx, sy, sz)) {
                                    let cheb = (sx - x).abs().max((sy - y).abs()).max((sz - z).abs());
                                    assert!(k <= cheb, "chunk ({x},{y},{z}) claims {k} but a surface is {cheb} away");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
