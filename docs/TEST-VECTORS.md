# DMR Encoder Test Vector Coverage

Reference for the golden vectors validating our DMR FEC / BPTC /
embedded-LC encoders against independent sources.  Complements the
actual tests under `dmr-wire/src/*/tests` modules.

## Summary

Columns: ETSI normative, Python references, live BM wire captures,
self-consistency tests, decoder round-trip + bit-flip recovery
sweeps.

| Layer                      | ETSI | Python | Wire | Self-consistency | Decoder sweep |
|----------------------------|:-:|:-:|:-:|:-:|:-:|
| Golay(20,8,7)              |   | 2 entries |   |   | 256 inputs RT, 1-bit detect |
| QR(16,7,6)                 |   | 2 vectors |   |   | 128 inputs RT, mismatch detect |
| Hamming(15,11,3)           |   | 1 vector |   | zeros, ones | every-bit flip recover |
| Hamming(13,9,3)            |   | 3 vectors |   | zeros, ones | every-bit flip recover, 2 unused syndromes detected |
| Hamming(16,11,4)           |   | 2 vectors |   | zeros, ones | every-bit flip recover, all 120 double-error pairs detected |
| CRC-5 (embedded LC)        |   | 1 vector |   | zeros, mod 31 |   |
| RS(12,9)                   | 9 gen rows | 4 vectors | 11 bursts |   | syndromes zero on valid; 3060 single-byte errors detected |
| BPTC(196,96) encoder       | interleave + data positions | dmrpy data_head | 11 bursts |   |   |
| BPTC(196,96) decoder       | | 2 ok-dmrlib bursts + dmrpy data_head | 6 BM captures | RT | 196 bit-flip recovers via row+col Hamming |
| Embedded LC encoder        |   | dmrpy ETSI B.2 example |   | parity invariants |   |
| Embedded LC decoder        |   | dmrpy VOICE_SUPERFRAME |   | RT | 128 bit-flip recovers; double-row-error detected |
| Slot type Golay(20,8)      |   |   | 11 bursts |   |   |

## Sources

### Normative (ETSI TS 102 361-1 V2.5.1)

Download from <https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/>.

- **Table B.18** (RS(12,9) generator matrix): the 9 unit-vector
  messages `m = e_i` give us the parity on row `i`, columns 9..12.
  Any linear RS(12,9) encoder matching all 9 rows matches spec for
  every input.  Exercised by `dmr_wire::rs::tests::rs_generator_matrix_unit_vectors`.
- **Table B.2** (BPTC interleaving indices): our formula
  `(i * 181) % 196` must agree with every row of the normative
  table.  Sampled at row/column boundaries in
  `dmr_wire::bptc::tests::bptc_interleave_formula_matches_spec`.
- **Table B.2** implicit data bit placement: `DATA_POSITIONS` must
  place `I(n)` at the matrix index column B.2 assigns.  Spot-checked
  in `dmr_wire::bptc::tests::bptc_data_positions_match_spec`.
- **Table B.21** (Data Type CRC Mask): `LC_HEADER_MASK = 0x969696`
  and `LC_TERMINATOR_MASK = 0x999999` match `dmr_wire::rs`.

### Python reference implementations

- **thomastoye/dmr-from-scratch** ("dmrpy"): Python/Jupyter DMR
  reference.  Primary source for:
  - Embedded LC end-to-end worked example (ETSI TS 102 361-1
    B.2).  9-byte LC -> 4 x 32-bit fragments `[0x4E0F0606,
    0x17110047, 0x0C03181B, 0x175A0F4E]` with CRC-5 `0xC`.  One
    assertion exercises CRC-5 + LC bit placement + Hamming(16,11,4)
    row parity + column parity + column serialization + fragment
    packing.  In `dmr_wire::embedded_lc::tests::fragments_match_dmrpy_reference`.
  - VOICE_SUPERFRAME burst-to-LC end-to-end: dmrpy's 4 wire bursts
    B-E feed sync-section extraction + EMB header strip + 4-fragment
    concatenation + decode_raw, recovering the same 9-byte LC.  In
    `dmr_wire::embedded_lc::tests::embedded_lc_reconstructs_dmrpy_voice_superframe`.
    (Caveat: dmrpy's `test_create_from_superframe` asserts a different
    LC than the embedded fragments actually decode to; the file is
    marked TODO/experimentation.  We match the working half.)
  - Voice LC HEADER burst (VOICE_SUPERFRAME data_head): decoder
    extracts the same Full LC, encoder reproduces the wire info bits
    byte-exact.  In `dmr_wire::bptc::tests::bptc_encode_matches_dmrpy_data_head`
    -- the BPTC(196,96) encoder's first non-BM Python ground truth.
  - QR(16,7,6) at `info=9` (cc=1, lcss=1) = `0x1391`.  In
    `dmr_wire::sync::tests::emb_cc1_lcss1_matches_dmrpy`.
- **OK-DMR/ok-dmrlib**: actively maintained Python DMR library.
  Primary source for:
  - RS(12,9): 4 codewords spanning both mask types.  In
    `dmr_wire::rs::tests::rs_okdmrlib_reference_vectors`.
  - Hamming(15,11,3) / (13,9,3) / (16,11,4) valid codewords.  In
    `dmr_wire::fec::tests::hamming_*_okdmrlib_vectors`.
  - BPTC wire-burst round-trip: two real 33-byte data+control
    bursts.  Decode -> re-encode must return the original 196 bits.
    In `dmr_wire::bptc::tests::bptc_roundtrips_okdmrlib_wire_bursts`.

### Real Brandmeister wire captures

6 voice LC headers + 5 matching terminators captured from TG 91 on
2026-04-15 and 2026-04-16.  All group calls at color code 1.  Source
IDs: 3151238, 5201886, 3221731, 3222582, 2342361, 3023805.

- Encoder byte-exact on all 11 bursts:
  - `dmr_wire::bptc::tests::compare_against_bm_capture` (original single
    capture, kept for narrative clarity)
  - `dmr_wire::bptc::tests::bptc_encode_matches_bm_header_captures`
  - `dmr_wire::bptc::tests::bptc_encode_matches_bm_terminator_captures`
- Decoder extracts the known LC body (FLCO=group, FID=0, svc=0,
  dst=91, known src) for every captured header:
  - `dmr_wire::bptc::tests::bptc_decode_bm_capture_yields_known_lc_bytes`
  - `dmr_wire::bptc::tests::bptc_decode_bm_captures_yield_group_call_lc`

Each capture validates the entire pipeline end-to-end: BPTC(196,96)
interleave + Hamming row/column parity + RS(12,9) + data-type mask +
slot-type Golay(20,8) encoding.

### Self-consistency

- `dmr_wire::bptc::tests::bptc_encode_decode_roundtrip`: encoder+decoder
  are strict inverses on arbitrary 96-bit inputs (independent of any
  external reference).
- `dmr_wire::embedded_lc::tests::encode_raw_parity_holds`: after
  encoding non-trivial data and reversing the column serialization,
  every row's Hamming(16,11,4) parity and row 7's column parity
  still hold.
- `dmr_wire::frame::tests::assemble_extract_roundtrip`: voice burst
  assemble and extract are inverses.
- `dmr_wire::frame::tests::pack_unpack_dibits_roundtrip`: 36-dibit pack
  and unpack are inverses (the dibit layout used by AmbeFrame).

### Decoder round-trip and bit-flip recovery (cfg(test))

These verify our encoders are invertible across the entire input
space (not just at single-vector points) and that bit errors
within FEC capacity are recovered.  Each decoder is `cfg(test)`
only; promote to runtime when an RF reception path needs them.

- `dmr_wire::bptc::tests::bptc_encode_decode_roundtrip`: encoder +
  decoder are strict inverses on arbitrary 96-bit inputs.
- `dmr_wire::bptc::tests::bptc_decode_correct_recovers_single_bit_errors`:
  every one of the 196 bit positions can be flipped and recovered
  via row Hamming(15,11,3) + column Hamming(13,9,3) correction.
- `dmr_wire::embedded_lc::tests::decode_raw_clean_round_trip` /
  `decode_raw_recovers_single_bit_errors`: same for BPTC(128,77).
- `dmr_wire::embedded_lc::tests::decode_raw_detects_double_errors_in_a_row`:
  Hamming(16,11,4) distance-4 detects 2-bit errors in a single row.
- `dmr_wire::fec::tests::hamming_*_correct_flips_every_bit`: each
  Hamming variant recovers from any single-bit flip.
- `dmr_wire::fec::tests::hamming_16_11_correct_detects_double_errors`:
  all 120 = C(16,2) two-bit flip pairs detected as Err.
- `dmr_wire::fec::tests::golay_20_8_round_trip_all_inputs` (256 inputs)
  and `dmr_wire::fec::tests::qr_16_7_round_trip_all_inputs` (128
  inputs): table-inverse decoders verify every encoder output.
- `dmr_wire::fec::tests::golay_20_8_decode_detects_single_bit_flip`:
  every 1-bit flip in a Golay(20,8) codeword detected as Err.
- `dmr_wire::rs::tests::rs_syndromes_zero_for_valid_codewords`:
  syndrome calculator returns zero for our encoder's outputs
  across 12 sample messages (zeros, ones, all 9 generator
  unit-vectors, BM TG 91 LC).
- `dmr_wire::rs::tests::rs_syndromes_detect_single_byte_error`: every
  one of 12 x 255 = 3060 single-byte error patterns yields
  non-zero syndromes (RS(12,9,4) distance 4).

### End-to-end integration tests

Per-handler tests exercise `PttMachine::on_dmrd` / `on_usrp` in
isolation; these tests spawn the full `voice_task` and drive events
through real mpsc channels, so the select-loop dispatch,
spawn_blocking vocoder offload, cancel plumbing, and channel-close
shutdown all run together.  Stub vocoder returns fixed PCM / AMBE.

- `dmr_wire::voice::tests::integration_full_rx_call`: DMRD header +
  voice burst + terminator -> 3 USRP voice frames + 1 unkey.
- `dmr_wire::voice::tests::integration_full_tx_call`: USRP keyup +
  3 voice frames + unkey -> DMRD header + voice burst + terminator
  all sharing one stream_id.
- `dmr_wire::voice::tests::integration_shutdown_in_rx_emits_unkey`:
  cancel from Rx state emits a final USRP unkey via on_shutdown.
- `dmr_wire::voice::tests::integration_shutdown_in_tx_emits_terminator`:
  cancel from Tx state flushes any partial burst and emits a DMRD
  terminator.

## Uncovered

These are low-priority until a real use case or capture surfaces:

- **Private calls (FLCO=3)**: our unit tests exercise the private
  branch of `build_voice_lc`, but no live wire capture validates the
  full burst.  A captured unit-to-unit call on DMR would close this.
- **Non-default color codes (cc != 1)**: spec-covered via the slot-
  type Golay encoding logic, but no wire capture.  Any captured
  burst at cc in 2..=15 would validate.
- **Reverse channel (burst F)**: we emit null embedded signalling
  (LCSS=0) on burst F.  No known RC-content wire captures.
- **AMBE codeword Golay(24,12) + Golay(23,12) FEC** on inbound
  voice frames is intentionally not handled: the vocoder
  (mbelib / ThumbDV / AMBEserver) does its own Golay decode +
  PRNG demodulation internally.  We extract the 49 source bits
  systematically and pass through.  Adding our own Golay layer
  would duplicate the vocoder's work and require Golay(24,12) +
  Golay(23,12) encoder + decoder + vector sources -- ~250 LOC
  for no immediate benefit.
- **Full single-byte RS(12,9) error correction** (Berlekamp-Massey
  / PGZ + Forney) is not implemented.  Syndrome detection is
  enough to flag a corrupt LC; correction would let us recover one
  byte per codeword.  Add if an RF reception path wants it.
- **All decoders are `cfg(test)` only** -- no runtime consumer.
  Promote to `pub(crate)` when an RF reception path needs them.

## How to add a new wire capture

1. Re-add the `dmr_data = hex::encode(pkt.dmr_data)` field to the
   `RX header` / `RX terminator` log lines in `dmr-wire/src/voice.rs`.
2. Run the bridge with `RUST_LOG=info cargo run --features mbelib
   config.toml |& grep dmr_data | tee /tmp/l`.
3. Extract `(src_id, dst_id, dmr_data)` tuples from the log.
4. Add them to `BM_CAPTURES` in `dmr-wire/src/bptc.rs` tests.
5. Revert the log change.
