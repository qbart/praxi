# Benchmark results (`bench.json`)

`praxi bench --json` writes its results as one UTF-8 JSON object
([benchmarks.md](../features/benchmarks.md)). The same file is read back by `--baseline`.
`bench/baseline.json` in the repository is such a file.

## Version

The top-level `version` is `1`. A reader rejects other versions. Fields may be added
without changing the version; readers ignore unknown fields.

## Object

```json
{
  "version": 1,
  "input": "big_buck_bunny_1080_10s.mp4",
  "width": 1920,
  "height": 1080,
  "fps_num": 60,
  "fps_den": 1,
  "frames": 180,
  "settings": {
    "quality": "fast",
    "rdo": 0.0,
    "keyint": 60,
    "skip_threshold": 3
  },
  "encode_seconds": 21.3,
  "variants": [
    {
      "format": "bc7",
      "bytes": 260000000,
      "mb_per_s": 86.7,
      "skipped_pct": 30.1,
      "keyframes": 3,
      "psnr_db": 41.2,
      "psnr_min_db": 38.9,
      "decode_ms": { "mean": 0.41, "p95": 0.62, "max": 1.9 }
    }
  ]
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | integer | `1` |
| `input` | string | File name of the input, without directories. |
| `width`, `height` | integer | Video size in pixels. |
| `fps_num`, `fps_den` | integer | Frame rate as a fraction. |
| `frames` | integer | Frames encoded. |
| `settings.quality` | string | `fast`, `medium` or `thorough`. |
| `settings.rdo` | number | UASTC RDO lambda for BC7 (`0` = off). |
| `settings.keyint` | integer | Frames between keyframes. |
| `settings.skip_threshold` | integer | Temporal skip threshold. |
| `encode_seconds` | number | Wall-clock encode time, all variants. |
| `variants` | array | One object per variant, in `--variant` order. |

### Variant

| Field | Type | Meaning |
|---|---|---|
| `format` | string | `bc7`, `astc4x4`, `astc6x6` or `astc8x8`, as in `--variant`. |
| `bytes` | integer | Bytes of the variant's video chunks, after compression. |
| `mb_per_s` | number | `bytes / 1e6` per second of encoded duration. |
| `skipped_pct` | number | Temporally skipped blocks, percent of all blocks of delta and key frames. |
| `keyframes` | integer | Keyframes written. |
| `psnr_db` | number | PSNR over all frames, capped at 100. |
| `psnr_min_db` | number | PSNR of the worst frame, capped at 100. |
| `decode_ms.mean` | number | Mean time of one player `advance`, decoding one frame, in ms. |
| `decode_ms.p95` | number | 95th percentile (nearest rank) of the same. |
| `decode_ms.max` | number | Maximum of the same. |

Numbers are written with full precision. Comparison rules are in
[benchmarks.md](../features/benchmarks.md#baseline-comparison).
