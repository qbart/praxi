//! CPU decoding of frames for tooling (`docs/features/frame-export.md`).

use crate::{Error, Result};
use praxi_format::{TextureFormat, VideoParams};

/// Decodes a frame's block data to `width * height * 4` bytes of RGBA8, cropping padding.
/// BC4 and BC5 decode as stored: red, and red plus green, with alpha 1.
pub fn decode_frame(params: &VideoParams, data: &[u8]) -> Result<Vec<u8>> {
    if data.len() != params.frame_size() {
        return Err(Error::Encode(format!("frame has {} bytes, expected {}", data.len(), params.frame_size())));
    }
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
