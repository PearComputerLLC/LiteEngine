//! 3D model decoding — `.obj` and glTF (`.glb`/`.gltf`) into [`ModelData`].
//!
//! Both formats decode to the engine's shared interleaved layout
//! (`pos3 normal3 uv2` + `u16` indices), so a streamed model costume uploads
//! straight into the legacy rasterizer *and* the wgpu PBR pipeline.
//!
//! * **OBJ** via `tobj`: all meshes concatenated, negative indices resolved
//!   by the loader, missing normals re-computed (smooth), missing UVs zeroed.
//! * **glTF** via the `gltf` crate: `import_slice` reads `.glb` (and
//!   `.gltf` with embedded data URIs) from memory — a requirement because
//!   assets stream out of zip packages, not loose files. Every mesh node of
//!   the default scene is baked in with its world transform applied.
//!
//! The engine caps models at 65 535 vertices (the u16 index budget shared
//! with the GPU pipelines); larger files fail with a descriptive error so
//! games get a readable message instead of a silent corruption.

use crate::snapshot::ModelData;

/// Hard vertex budget (u16 indices).
pub const MAX_VERTICES: usize = 65_535;

/// Decode a model from raw bytes, dispatching on the file extension.
pub fn decode_model(path: &str, bytes: &[u8]) -> Result<ModelData, String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".obj") {
        decode_obj(bytes)
    } else if lower.ends_with(".glb") {
        decode_gltf(bytes)
    } else if lower.ends_with(".gltf") {
        decode_gltf(bytes)
    } else {
        Err(format!(
            "unsupported model format '{path}' (expected .obj, .glb or .gltf)"
        ))
    }
}

// --------------------------------------------------------------------- OBJ

fn decode_obj(bytes: &[u8]) -> Result<ModelData, String> {
    let obj = tobj::load_obj_buf(
        &mut std::io::Cursor::new(bytes),
        &tobj::LoadOptions {
            single_index: true,
            triangulate: true,
            ..Default::default()
        },
        // Materials are ignored (flat-tint rendering); satisfy the loader.
        |_material_path| Ok((Vec::new(), Default::default())),
    )
    .map_err(|e| format!("obj decode failed: {e}"))?;

    let mut out = ModelData::default();
    for model in &obj.0 {
        let mesh = &model.mesh;
        let pos = &mesh.positions;
        let n_pos = pos.len() / 3;
        let normals = &mesh.normals;
        let texcoords = &mesh.texcoords;
        let base = out.vertex_count();

        for v in 0..n_pos {
            let (nx, ny, nz) = if normals.len() >= (v + 1) * 3 {
                (normals[v * 3], normals[v * 3 + 1], normals[v * 3 + 2])
            } else {
                (0.0, 0.0, 0.0)
            };
            let (u, vv) = if texcoords.len() >= (v + 1) * 2 {
                (texcoords[v * 2], 1.0 - texcoords[v * 2 + 1])
            } else {
                (0.0, 0.0)
            };
            out.vertices.extend_from_slice(&[
                pos[v * 3], pos[v * 3 + 1], pos[v * 3 + 2], nx, ny, nz, u, vv,
            ]);
        }
        // tobj `single_index` yields per-corner indices already offset per
        // mesh; remap into our concatenated buffer (u16 budget enforced).
        for i in &mesh.indices {
            let idx = base as u32 + i;
            let idx16 = u16::try_from(idx)
                .map_err(|_| format!("obj exceeds the {} vertex budget", MAX_VERTICES));
            out.indices.push(idx16?);
        }
    }

    if out.vertex_count() == 0 {
        return Err("obj contains no vertices".into());
    }
    if out.vertex_count() > MAX_VERTICES {
        return Err(format!(
            "obj has {} vertices, over the {} budget — decimate the mesh",
            out.vertex_count(),
            MAX_VERTICES
        ));
    }
    // Any vertex without normals (loader leaves zeros) gets smooth normals.
    if obj.0.iter().all(|m| m.mesh.normals.is_empty()) {
        out.compute_normals();
    }
    Ok(out)
}

// -------------------------------------------------------------------- glTF

fn decode_gltf(bytes: &[u8]) -> Result<ModelData, String> {
    let (document, buffers, _images) = gltf::import_slice(bytes)
        .map_err(|e| format!("glTF decode failed: {e}"))?;

    let mut out = ModelData::default();

    // Walk the default scene (or all scenes when none is marked), baking
    // each mesh node with its world transform applied to positions/normals.
    for scene in document.scenes() {
        for node in scene.nodes() {
            bake_node(&node, &buffers, &mut out, Mat4::IDENTITY)?;
        }
    }

    if out.vertex_count() == 0 {
        return Err("glTF contains no vertices".into());
    }
    Ok(out)
}

/// Column-major 4x4 (matches glTF's memory layout).
#[derive(Clone, Copy)]
struct Mat4([[f32; 4]; 4]);

impl Mat4 {
    const IDENTITY: Mat4 = Mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    fn mul(&self, o: &Mat4) -> Mat4 {
        let (a, b) = (&self.0, &o.0);
        let mut m = [[0.0f32; 4]; 4];
        for col in 0..4 {
            for row in 0..4 {
                m[col][row] = a[0][row] * b[col][0]
                    + a[1][row] * b[col][1]
                    + a[2][row] * b[col][2]
                    + a[3][row] * b[col][3];
            }
        }
        Mat4(m)
    }

    fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        let w = m[3][0] * p[0] + m[3][1] * p[1] + m[3][2] * p[2] + m[3][3];
        let out = [
            m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
            m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
            m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
        ];
        out.map(|v| if w.abs() > 1e-9 { v / w } else { v })
    }

    /// Rotate-only application (normals; assumes uniform-ish scaling).
    fn vector(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
            m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
            m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
        ]
    }
}

fn bake_node(
    node: &gltf::Node,
    buffers: &[gltf::buffer::Data],
    out: &mut ModelData,
    parent: Mat4,
) -> Result<(), String> {
    let world = parent.mul(&Mat4(node.transform().matrix()));
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue; // skip lines/points/fan topologies
            }
            let reader =
                prim.reader(|buffer| buffers.get(buffer.index()).map(|b| b.0.as_slice()));
            let pos = reader
                .read_positions()
                .ok_or("glTF primitive has no POSITION accessor")?;
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(Iterator::collect)
                .unwrap_or_default();
            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|tc| {
                    tc.into_f32()
                        .map(|[u, v]| [u, 1.0 - v])
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let base = out.vertex_count() as u32;
            let verts: Vec<[f32; 3]> = pos.collect();
            let count = verts.len() as u32;
            for (i, p) in verts.iter().enumerate() {
                let wp = world.point(*p);
                let (nx, ny, nz) = match normals.get(i) {
                    Some(n) => {
                        let wn = world.vector(*n);
                        let len = (wn[0] * wn[0] + wn[1] * wn[1] + wn[2] * wn[2])
                            .sqrt()
                            .max(1e-9);
                        (wn[0] / len, wn[1] / len, wn[2] / len)
                    }
                    None => (0.0, 0.0, 0.0),
                };
                let uv = uvs.get(i).copied().unwrap_or([0.0, 0.0]);
                out.vertices
                    .extend_from_slice(&[wp[0], wp[1], wp[2], nx, ny, nz, uv[0], uv[1]]);
            }
            if let Some(indices) = reader.read_indices() {
                for i in indices.into_u32() {
                    let idx = base + i;
                    let idx16 = u16::try_from(idx)
                        .map_err(|_| format!("glTF exceeds the {} vertex budget", MAX_VERTICES));
                    out.indices.push(idx16?);
                }
            } else {
                for i in 0..count {
                    let idx = base + i;
                    let idx16 = u16::try_from(idx)
                        .map_err(|_| format!("glTF exceeds the {} vertex budget", MAX_VERTICES));
                    out.indices.push(idx16?);
                }
            }
        }
    }
    for child in node.children() {
        bake_node(&child, buffers, out, world)?;
    }
    if out.vertex_count() > MAX_VERTICES {
        return Err(format!(
            "glTF has {} vertices, over the {} budget — decimate the mesh",
            out.vertex_count(),
            MAX_VERTICES
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_cube_parses() {
        // A minimal two-triangle OBJ quad.
        let src = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1 4//1\n";
        let m = decode_obj(src).expect("decode");
        assert_eq!(m.vertex_count(), 4);
        assert_eq!(m.index_count(), 6);
        // Normals present in the file, so none are synthesized.
        assert_eq!(m.vertices[3], 0.0);
        assert_eq!(m.vertices[5], 1.0);
    }

    #[test]
    fn obj_without_normals_gets_smooth_normals() {
        let src = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let m = decode_obj(src).expect("decode");
        let n = &m.vertices[3..6];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5, "normal not normalized: {n:?}");
    }

    #[test]
    fn rejects_unknown_format() {
        assert!(decode_model("mesh.fbx", b"junk").is_err());
    }
}
