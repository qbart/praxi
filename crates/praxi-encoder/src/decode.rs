//! CPU decoding of frames for tooling (`docs/features/frame-export.md`).

use crate::{Error, Result};
use ktx::astcenc::{self, AstcencImage, AstcencImageData, AstcencProfile, AstcencSwizzle};
use ktx::basisu::gpu_texture;
use praxi_format::{TextureFormat, VideoParams};

/// Decodes a frame's block data to `width * height * 4` bytes of RGBA8, cropping padding.
pub fn decode_frame(params: &VideoParams, data: &[u8]) -> Result<Vec<u8>> {
    if data.len() != params.frame_size() {
        return Err(Error::Encode(format!("frame has {} bytes, expected {}", data.len(), params.frame_size())));
    }
    let (w, h) = (params.width as usize, params.height as usize);
    match params.format {
        TextureFormat::Bc1 => Ok(decode_4x4(params, data, |block, px| {
            gpu_texture::unpack_bc1(block.try_into().expect("8-byte block"), px, true)
        })),
        TextureFormat::Bc7 => Ok(decode_4x4(params, data, |block, px| {
            gpu_texture::unpack_bc7(block.try_into().expect("16-byte block"), px)
        })),
        TextureFormat::Astc4x4 | TextureFormat::Astc5x5 | TextureFormat::Astc6x6 | TextureFormat::Astc8x8 => {
            let (bw, bh) = params.format.block_size();
            let fail = |what: &str, e: astcenc::AstcencError| Error::Encode(format!("astcenc {what}: {e:?}"));
            let cfg = astcenc::astcenc_config_init(
                AstcencProfile::LdrSrgb,
                bw,
                bh,
                1,
                astcenc::ASTCENC_PRE_FASTEST,
                astcenc::ASTCENC_FLG_DECOMPRESS_ONLY,
            )
            .map_err(|e| fail("config", e))?;
            let mut ctx = astcenc::astcenc_context_alloc(&cfg, 1).map_err(|e| fail("context", e))?;
            let mut image = AstcencImage {
                dim_x: w as u32,
                dim_y: h as u32,
                dim_z: 1,
                data: AstcencImageData::U8(vec![vec![0; w * h * 4]]),
            };
            astcenc::astcenc_decompress_image(&mut ctx, data, &mut image, &AstcencSwizzle::RGBA, 0)
                .map_err(|e| fail("decompress", e))?;
            match image.data {
                AstcencImageData::U8(mut planes) if planes.len() == 1 => Ok(planes.remove(0)),
                _ => Err(Error::Encode("astcenc returned an unexpected image layout".into())),
            }
        }
        f @ (TextureFormat::Bc3 | TextureFormat::Bc4 | TextureFormat::Bc5) => {
            Err(Error::Encode(format!("CPU decoding of {f} is not supported yet")))
        }
    }
}

/// Decodes 4x4-block formats with `unpack`, which fills 16 RGBA pixels from one block.
fn decode_4x4(params: &VideoParams, data: &[u8], unpack: impl Fn(&[u8], &mut [[u8; 4]; 16]) -> bool) -> Vec<u8> {
    let (w, h) = (params.width as usize, params.height as usize);
    let bb = params.format.block_bytes() as usize;
    let bx_count = params.blocks_x() as usize;
    let mut out = vec![0u8; w * h * 4];
    let mut px = [[0u8; 4]; 16];
    for (i, block) in data.chunks_exact(bb).enumerate() {
        // Invalid blocks decode as opaque magenta, like a missing texture.
        if !unpack(block, &mut px) {
            px = [[255, 0, 255, 255]; 16];
        }
        let (bx, by) = (i % bx_count * 4, i / bx_count * 4);
        for y in 0..4.min(h - by) {
            for x in 0..4.min(w - bx) {
                let at = ((by + y) * w + bx + x) * 4;
                out[at..at + 4].copy_from_slice(&px[y * 4 + x]);
            }
        }
    }
    out
}
