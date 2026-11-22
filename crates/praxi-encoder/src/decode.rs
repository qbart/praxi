//! CPU decoding of frames for tooling (`docs/features/frame-export.md`).

use crate::{Error, Result};
use praxi_format::{TextureFormat, VideoParams};

/// Decodes a frame's block data to `width * height * 4` bytes of RGBA8, cropping padding.
/// BC4 and BC5 decode as stored: red, and red plus green, with alpha 1. HDR frames take
/// [`decode_frame_hdr`].
pub fn decode_frame(params: &VideoParams, data: &[u8]) -> Result<Vec<u8>> {
    check_size(params, data)?;
    let format = match params.format {
        TextureFormat::Bc1 => imgio::Format::Bc1Srgb,
        TextureFormat::Bc3 => imgio::Format::Bc3Srgb,
        TextureFormat::Bc4 => imgio::Format::Bc4Unorm,
        TextureFormat::Bc5 => imgio::Format::Bc5Unorm,
        TextureFormat::Bc7 => imgio::Format::Bc7Srgb,
        f @ (TextureFormat::Astc4x4 | TextureFormat::Astc5x5 | TextureFormat::Astc6x6 | TextureFormat::Astc8x8) => {
            let (bw, bh) = f.block_size();
            let block = imgio::AstcBlock::from_dim(bw, bh).expect("ASTC block size");
            imgio::Format::Astc(block, imgio::AstcKind::Srgb)
        }
        TextureFormat::Motion => return Err(Error::Encode("motion frames are coded frames, not block data".into())),
        f @ (TextureFormat::Bc6h | TextureFormat::Astc4x4Hdr | TextureFormat::Astc6x6Hdr) => {
            return Err(Error::Encode(format!("{f} is HDR: decode it with decode_frame_hdr")));
        }
    };
    let (w, h) = (params.width, params.height);
    let decoded = imgio::decode_to_buffer(format, data, w, h).map_err(|e| Error::Encode(format!("{format:?}: {e}")))?;
    let pixels = (w * h) as usize;
    Ok(match format.decoded() {
        imgio::Format::R8Unorm => decoded[..pixels].iter().flat_map(|&r| [r, 0, 0, 255]).collect(),
        imgio::Format::Rg8Unorm => decoded[..pixels * 2].as_chunks::<2>().0.iter().flat_map(|c| [c[0], c[1], 0, 255]).collect(),
        _ => decoded[..pixels * 4].to_vec(),
    })
}

/// Decodes an HDR frame's block data to `width * height * 4` floats of linear RGBA (alpha 1),
/// in the stream's primaries, 1.0 at reference white (`docs/features/hdr.md#tools`).
pub fn decode_frame_hdr(params: &VideoParams, data: &[u8]) -> Result<Vec<f32>> {
    check_size(params, data)?;
    let format = match params.format {
        TextureFormat::Bc6h => imgio::Format::Bc6hUfloat,
        TextureFormat::Astc4x4Hdr => imgio::Format::Astc(imgio::AstcBlock::B4x4, imgio::AstcKind::Float),
        TextureFormat::Astc6x6Hdr => imgio::Format::Astc(imgio::AstcBlock::B6x6, imgio::AstcKind::Float),
        f => return Err(Error::Encode(format!("{f} is not HDR: decode it with decode_frame"))),
    };
    let decoded = imgio::decode_to_buffer(format, data, params.width, params.height)
        .map_err(|e| Error::Encode(format!("{format:?}: {e}")))?;
    let pixels = (params.width * params.height) as usize;
    Ok(decoded[..pixels * 8]
        .as_chunks::<8>()
        .0
        .iter()
        .flat_map(|p| [0, 2, 4].map(|c| imgio::half::to_f32(u16::from_le_bytes([p[c], p[c + 1]]))).into_iter().chain([1.0]))
        .collect())
}

fn check_size(params: &VideoParams, data: &[u8]) -> Result<()> {
    if data.len() != params.frame_size() {
        return Err(Error::Encode(format!("frame has {} bytes, expected {}", data.len(), params.frame_size())));
    }
    Ok(())
}
