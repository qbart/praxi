# praxi-format

Reader and writer for the `.praxi` container: header, stream table, LZ4-compressed chunks,
audio codecs (PCM, IMA ADPCM, Opus), cues and the motion codec's bitstream. Feature
`cue-sheet` reads and writes JSON cue sheets.

Most users want the [`praxi`](https://crates.io/crates/praxi) player instead; this crate is
what it and the encoder share. Implements file format 0.11.
