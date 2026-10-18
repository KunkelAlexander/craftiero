// Meshing one chunk with Surface Nets (via the fast-surface-nets crate).
//
// Why 18^3 and not 16^3: a mesh vertex lives in a cell between 8 voxels, and
// the triangles joining two cells need both. Reading one extra voxel on every
// side lets each chunk build its own triangles while neighbouring chunks still
// meet without a gap or an overlap.
//
// The mesh is a render-side CACHE of the grid, never read by the sim: collision
// samples the grid directly.

use crate::{Chunk, Grid, CHUNK, VOX};
use fast_surface_nets::ndshape::{ConstShape, ConstShape3u32};
use fast_surface_nets::{surface_nets, SurfaceNetsBuffer};
use glam::{IVec3, Vec3};

const PAD: i32 = CHUNK + 2;
type PaddedShape = ConstShape3u32<18, 18, 18>;

#[derive(Default)]
pub struct Mesh {
    /// World-space metres.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl Grid {
    /// Mesh of chunk `c`; empty if the chunk has no surface in it.
    pub fn mesh_chunk(&self, c: IVec3) -> Mesh {
        // A uniform chunk can still own triangles when a neighbour's voxels
        // cross zero in its border cells, so the early exit below looks at the
        // whole 18^3 input, not at the chunk's own kind.
        let origin = c * CHUNK - IVec3::ONE; // voxel coords of padded sample (0,0,0)
        let mut samples = [1.0f32; 18 * 18 * 18];
        let (mut any_neg, mut any_pos) = (false, false);
        for z in 0..PAD {
            for y in 0..PAD {
                for x in 0..PAD {
                    let v = crate::dequantize(self.voxel(origin + IVec3::new(x, y, z)));
                    any_neg |= v < 0.0;
                    any_pos |= v >= 0.0;
                    samples[PaddedShape::linearize([x as u32, y as u32, z as u32]) as usize] = v;
                }
            }
        }
        if !(any_neg && any_pos) {
            return Mesh::default();
        }

        let mut buffer = SurfaceNetsBuffer::default();
        surface_nets(&samples, &PaddedShape {}, [0; 3], [17; 3], &mut buffer);

        let shift = origin.as_vec3();
        Mesh {
            positions: buffer
                .positions
                .iter()
                .map(|p| ((Vec3::from(*p) + shift) * VOX).to_array())
                .collect(),
            normals: buffer.normals.clone(),
            indices: buffer.indices.clone(),
        }
    }

    /// True if chunk `c` holds real data (for stats).
    pub fn is_data(&self, c: IVec3) -> bool {
        matches!(self.chunks[self.chunk_index(c)], Chunk::Data(_))
    }
}

#[cfg(test)]
mod tests {
    use crate::{Grid, Op, Shape};
    use glam::{IVec3, Vec3};
    use std::collections::HashMap;

    /// A carved sphere is a closed surface, so every edge of the combined mesh
    /// of all chunks must be shared by exactly two triangles. Any seam between
    /// chunks would show up as an edge used once. The sphere straddles chunk
    /// borders on purpose (8 m is a chunk corner).
    #[test]
    fn chunk_meshes_join_without_gaps() {
        let mut g = Grid::new(IVec3::new(4, 4, 4));
        g.apply(Shape::Sphere { center: Vec3::splat(8.0), radius: 3.1 }, Op::Carve);
        let mut edges: HashMap<(u64, u64), u32> = HashMap::new();
        let mut total_tris = 0;
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut tris: Vec<[usize; 3]> = Vec::new();
        for c in g.take_dirty() {
            let m = g.mesh_chunk(c);
            let base = positions.len();
            positions.extend(m.positions.iter().copied());
            for t in m.indices.chunks(3) {
                tris.push([base + t[0] as usize, base + t[1] as usize, base + t[2] as usize]);
            }
        }
        // Weld vertices by position (chunks duplicate none, but keys keep the test honest).
        let key = |p: [f32; 3]| -> u64 {
            let q = |v: f32| ((v * 1000.0).round() as i64 & 0x1fffff) as u64;
            (q(p[0]) << 42) | (q(p[1]) << 21) | q(p[2])
        };
        for t in &tris {
            total_tris += 1;
            for i in 0..3 {
                let (a, b) = (key(positions[t[i]]), key(positions[t[(i + 1) % 3]]));
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        assert!(total_tris > 100, "got {total_tris} triangles");
        let bad = edges.values().filter(|&&n| n != 2).count();
        assert_eq!(bad, 0, "{bad} edges are not shared by exactly two triangles");
    }

    #[test]
    fn mesh_vertices_lie_on_the_surface() {
        let mut g = Grid::new(IVec3::new(4, 4, 4));
        g.apply(Shape::Sphere { center: Vec3::splat(8.0), radius: 3.0 }, Op::Carve);
        for c in g.take_dirty() {
            for p in g.mesh_chunk(c).positions {
                let d = (Vec3::from(p) - Vec3::splat(8.0)).length() - 3.0;
                assert!(d.abs() < crate::VOX, "vertex {p:?} is {d} m from the sphere");
            }
        }
    }
}
