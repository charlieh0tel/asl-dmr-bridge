# asl-dmr-bridge -- ASL3 to DMR Bridge

Bridge an FM repeater (via AllStarLink / ASL3) to a DMR network by acting as a
Homebrew-variant DMR peer. Initial target is Brandmeister; the network profile
is abstracted so other networks (DMR+, TGIF) can be added later.

---

## Physical Topology

```
FM Repeater --audio-->  ASL3 (Asterisk)
                          \- chan_usrp (UDP slin16)
                                |
                                v
                        [asl-dmr-bridge]
                         ambe crate <-> AMBE dongle
                                |
                                v BM Homebrew variant
                        DMR Network Master
                                |
                                v
                          DMR Repeater(s)
```

---

## Logical Architecture

```
ASL3 (Asterisk)
  \- chan_usrp (UDP slin16)
        |
        v
  [usrp]  <---------------------------------+
        |                                   |
        v                                   |
  [ambe crate]  <-> ThumbDV / AMBEserver    |
        |                                   |
        v                                   |
  [homebrew_client] <-> DMR Network           |
        |                                   |
        +-------- [ptt] ---------------------+
```

All components run concurrently via tokio tasks communicating over
`tokio::sync::mpsc` channels.

---

## Talkgroup / Slot Assignment

The bridge operates on a single configured (slot, talkgroup) pair. Both
directions (ASL3 -> DMR and DMR -> ASL3) use the same pair. Inbound traffic on
any other (slot, talkgroup) is silently dropped.

Configured via `[dmr]` section: `slot` (TS1 or TS2), `talkgroup`.

---

## Prior Art and References

Reference material, not runtime dependencies.

| Project | URL | Used for |
|---|---|---|
| HBlink3 | https://github.com/HBLink-org/hblink3 | Homebrew DMR protocol reference; `hblink.py` for auth handshake and RPTC format; `dmr_utils.py` for frame assembly constants |
| DMRGateway | https://github.com/g4klx/DMRGateway | **Primary reference** for both the Homebrew wire protocol (RPTC format used by Pi-Star/WPSD) and for DMR Layer 2 burst handling in software: `DMRData.cpp` extracts the 108-bit payload halves from the 33-byte `dmr_data`; `DMRSlotType.cpp` decodes the slot-type field (header / terminator / voice A-F); `DMREMB.cpp` and `DMREmbeddedData.cpp` parse embedded signaling across voice superframes; `DMRFullLC.cpp` handles the LC in voice header/terminator.  Talks Homebrew DMRD on the peer side (our exact context), so its burst-handling code is isolated from radio timing.  Does not decode AMBE audio. |
| MMDVMHost | https://github.com/g4klx/MMDVMHost | MMDVM host process; defines local DMRC format (unrelated to the BM wire protocol).  `DMRSlot.cpp` is a cross-check for DMRGateway's burst disassembly (same author, same DMR Layer 2 classes, but tangled with modem IO).  `AMBEFEC.cpp` has the DMR_A/B/C_TABLE position tables and `regenerateDMR()` which runs Golay(24,12) + Hamming + PRNG whitening on voice codewords. |
| ambe-server | https://github.com/f4fxl/ambe-server | AMBE dongle daemon; provides PCM<->AMBE+2 transcoding (UDP, DV3000 packets) |
| serialDV | https://github.com/f4exb/serialDV | C++ reference for the AMBE3000 serial wire protocol (packet mode, encode/decode transactions) -- useful when reviewing `ambe/src/thumbdv.rs` / `ambe/src/dv3000.rs` against known-good behaviour. Scope is the codec chip only; no DMR burst disassembly. |
| AMBETools (G4KLX) | https://github.com/g4klx/AMBETools | `AMBE2WAV` / `WAV2AMBE` / `AMBE2DVTOOL` CLI utilities. Useful for cross-checking our `.amb` handling, confirming AMBE3000 per-mode RATEP/FEC settings, and producing reference WAVs from `.amb` captures (cross-decoder sanity, not ground truth). |
| OP25 | https://github.com/osmocom/op25/tree/master/op25/gr-op25_repeater/lib | Alternative AMBE+2 reference implementation (`ambe_encoder.cc`, `imbe_decoder.cc`, `software_imbe_decoder.cc`); `dmr_const.h` for DMR sync/EMB constants; `dmr_bs_tx_bb_impl.cc` is the TX (burst assembly) side -- symmetric to the RX disassembly we need. |
| ASL-Asterisk / chan_usrp | https://github.com/AllStarLink/ASL-Asterisk/blob/develop/asterisk/channels/ | Authoritative USRP protocol definition: `chan_usrp.h`, `chan_usrp.c` |
| ETSI TS 102 361-1 | https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/ | DMR Layer 2 framing spec; authoritative reference for dmr_data field layout |
| DSD (szechyjs) | https://github.com/szechyjs/dsd | Digital Speech Decoder (ISC license, same author as mbelib).  `dmr_const.h` has the rW/rX/rY/rZ deinterleave tables (36 entries each) that map 36 dibits from the DMR voice burst into the `ambe_fr[4][24]` structure.  `dmr_voice.c` shows the physical burst layout: Frame1(36 dibits) + Frame2a(18) + SYNC/EMB(24) + Frame2b(18) + Frame3(36) = 132 dibits = 264 bits = 33 bytes. |
| ok-dmrlib | https://github.com/OK-DMR/ok-dmrlib | Python DMR library (AGPL-3.0) with complete ETSI FEC implementations (Golay, Hamming, BPTC, QR, RS, Trellis), PCAP tools, and 95% test coverage.  `test_burst.py` has hex-encoded DMRD test packets with voice bursts A-F -- useful as test vectors.  Code cannot be ported (AGPL), but test data (captured RF) and algorithm understanding are fine. |
| MMDVM-Dissector | https://github.com/marrold/MMDVM-Dissector | Wireshark dissector for MMDVM protocol.  Includes `mmdvm_example.pcap` -- a captured packet trace that likely contains DMRD voice frames.  Useful for generating test vectors.  CC BY-NC-SA 4.0. |
| kb9mwr AMBE notes | https://www.qsl.net/kb9mwr/projects/dv/codec/ambe.html | Background on AMBE+2 patent/licensing situation and links to open-source decoders. Not a bit-layout reference -- AMBE+2 internals are not publicly documented. |

---

## Component Specifications

### USRP Protocol (ASL3 Interface)

USRP is an AllStarLink-specific UDP audio protocol (unrelated to Ettus Research
USRP SDR hardware despite the shared name). The authoritative definition is
`chan_usrp.h` / `chan_usrp.c` in the ASL-Asterisk repo.

A packet is a 32-byte header followed by an optional audio payload. All u32
header fields are network byte order (big-endian, via htonl/ntohl in C).
Audio samples are native byte order (raw memcpy in C).

Header layout (`struct _chan_usrp_bufhdr`):
```
eye[4]        b"USRP"       magic / verification string
seq[4]        u32            sequence number (0 bypasses loss detection)
memory[4]     u32            memory ID (zero default)
keyup[4]      u32            PTT state: 1 = keyed, 0 = unkeyed
talkgroup[4]  u32            trunk TG id
type[4]       u32            0 = USRP_TYPE_VOICE, 1 = USRP_TYPE_DTMF, 2 = USRP_TYPE_TEXT
mpxid[4]      u32            reserved for future use
reserved[4]   u32            reserved for future use
```

Voice payload: 160 x i16 = 320 bytes (20 ms at 8 kHz slin16).

Unkey (PTT release): header-only packet with keyup=0, no audio payload.

No existing Rust crate implements this protocol. See `bridge/src/usrp.rs`.

Configuration: `local_host`, `local_port`, `remote_host`, `remote_port`,
`byte_swap` (for cross-endian peers).

---

### AMBE Transcoder

PCM <-> AMBE+2 transcoding. Implemented as a separate workspace crate (`ambe`)
that abstracts over three backends behind a `Vocoder` trait:

- **ThumbDV** (serial): DVSI AMBE-3000 over USB-serial, DV3000 packet protocol.
- **AMBEserver** (UDP): network client for an existing AMBEserver daemon,
  same DV3000 packet protocol but over UDP (default port 2460).
- **mbelib** (software, feature-gated): decode-only software vocoder via FFI.
  Encode not supported (mbelib encode quality is too poor for on-air use).

DV3000 packet format (shared by ThumbDV and AMBEserver):
- Start byte 0x61, 2-byte big-endian payload length, 1-byte type.
- Types: 0x00 control, 0x01 AMBE, 0x02 audio.
- AMBE+2 for DMR uses a specific RATEP (3600x2450) control configuration.

Frame sizes:
- PCM: 160 samples x i16 = 320 bytes (20 ms at 8 kHz)
- AMBE+2: 9 bytes (72 bits)

---

### Homebrew-Variant DMR Protocol (BM)

See `bridge/src/homebrew_client.rs`. Implements the client side of the Homebrew-variant
protocol used by DMRGateway/WPSD/Pi-Star when connecting to Brandmeister.

BM uses Homebrew-style RPTL/RPTK/RPTC auth and RPTPING keepalive, but it is
NOT pure HBlink3 Homebrew -- BM validates the RPTC config content beyond
what HBlink3 requires. Default/placeholder values are rejected.

#### Auth Handshake

```
Client -> Server:  RPTL + repeater_id[4]
Server -> Client:  RPTACK + nonce[4]
Client -> Server:  RPTK + repeater_id[4] + SHA256(nonce + password)[32]
Server -> Client:  RPTACK  (or MSTNAK on failure)
Client -> Server:  RPTC + config_packet[302]
Server -> Client:  RPTACK  (or MSTNAK on failure)
```

Auth hash is plain SHA256 (NOT HMAC). 4-byte raw nonce is prepended to the
password bytes, SHA256'd, 32-byte raw digest sent in RPTK.

#### RPTC Config Packet (302 bytes total)

Format matches DMRGateway's `getConfig()` sprintf:
`%8.8s%9.9s%9.9s%2.2s%2.2s%+08.4f%+09.4f%03d%-20.20s%-19.19s%c%-124.124s%40.40s%40.40s`

Fields (all fixed-width, left-aligned space-pad for strings, zero-pad for
numbers, except lat/lon which are %+0N.4f format with explicit sign):

```
RPTC            [4]   tag
repeater_id     [4]   big-endian u32
callsign        [8]   ASCII, space-padded
rx_freq         [9]   ASCII digits, e.g. "434000000"
tx_freq         [9]   ASCII digits, e.g. "439000000"
tx_power        [2]   ASCII digits, e.g. "01"
color_code      [2]   ASCII digits, e.g. "01"
latitude        [8]   e.g. "+00.0000"
longitude       [9]   e.g. "+000.0000"
height          [3]   ASCII digits, e.g. "000"
location        [20]  ASCII, space-padded
description     [19]  ASCII, space-padded
slots           [1]   ASCII digit ('1', '2', '3', or '4'; see below)
url             [124] ASCII, space-padded (may be empty)
software_id     [40]  ASCII, space-padded
package_id      [40]  ASCII, space-padded
```

**BM validates these fields more strictly than HBlink3.** Empirically
determined by bisection testing against a real BM master:
- `software_id`: must match `/YYYYMMDD_*/` (8-digit date prefix + `_`
  + anything).
- `package_id`: must start with `MMDVM`.
- `rx_freq` / `tx_freq`: must be non-zero 9-digit Hz values. Zero or
  obviously-fake values (e.g. `000000001`) trigger a "wrong configuration"
  warning; true zeros cause MSTCL (disconnect).
- `slots`: ASCII digit per MMDVMHost convention -- `'1'` (duplex,
  TS1 only), `'2'` (duplex, TS2 only), `'3'` (duplex, both), `'4'`
  (simplex hotspot).  This bridge runs single-slot, so the byte is
  derived from `[dmr] slot` and is always `'1'` or `'2'`.
  (Earlier docs claimed BM rejects anything but `'3'`; bisection
  testing 2026-05-01 against 3104.master.brandmeister.network
  showed `'1'` and `'2'` are both accepted -- prior claim was an
  over-generalization.)
- `longitude`: must include sign prefix (`+000.0000`).

Unchecked (any value accepted): description, location, url, power,
color_code, height.

#### Voice Frame -- DMRD (53 bytes)

```
b"DMRD"        [4]   magic
seq            [1]   0-255 wrapping
src_id         [3]   DMR source ID (repeater's registered DMR ID)
dst_id         [3]   destination TG
repeater_id    [4]
flags          [1]   slot (bit 7), call type (bit 6), frame type, data type
stream_id      [4]   unique per transmission
dmr_data      [33]   assembled DMR voice frame (see DMR Frame Assembly)
```

#### Keepalive

Send `RPTPING + repeater_id` every 5 seconds. Expect `MSTPONG`. Track last pong
time; reconnect if `keepalive_missed_limit` consecutive intervals pass without
a pong.

#### Disconnect

Send `RPTCL + repeater_id` on clean shutdown.

---

### DMR Voice Burst Disassembly (DMR -> ASL3)

The `dmr_data` field is **not** raw AMBE+2 bytes. It is a fully-assembled DMR
Layer 2 frame following ETSI TS 102 361-1 Section 9.1, with AMBE+2 codec bits
interleaved with sync patterns and embedded signaling.

#### Physical layout (33 bytes = 264 bits = 132 dibits)

```
Frame1       Frame2a    SYNC/EMB     Frame2b    Frame3
36 dibits    18 dibits  24 dibits    18 dibits  36 dibits
72 bits      36 bits    48 bits      36 bits    72 bits
```

Frame2 straddles the SYNC/EMB gap (36 + 36 = 72 bits).

#### AmbeFrame: raw on-air dibit stream

Each 72-bit AMBE codeword arrives bit-interleaved for RF protection.
We do **not** deinterleave in the burst-extraction path; instead
`AmbeFrame` carries the 36 on-air dibits packed MSB-first, 4 dibits
per byte (dibit 0 in bits 7..6), matching the DVSI/dsdcc convention.
This is what the AMBE-3000 chip expects: it deinterleaves and runs
FEC internally.

DMR deinterleave tables (rW/rX/rY/rZ) therefore live only in
`ambe/src/codeword.rs` -- invoked inside `extract_source_bits` for
the `mbelib` software backend, which operates on the 49 source bits
rather than the raw on-air stream.

Each 72-bit codeword = 49 source bits + 23 FEC bits:
- Row 0 (24 bits): Golay(24,12) -- 12 source (u0) + 12 parity
- Row 1 (23 bits): Golay(23,12) -- 12 source (u1) + 11 parity, PRNG whitened
- Rows 2-3 (25 bits): u2-u7 (25 source bits, unprotected)

`extract_source_bits` deinterleaves, dewhitens row 1 (PRNG seeded
from row 0 data bits, matching mbelib's
`mbe_demodulateAmbe3600x2450Data`), and reads source bits from HIGH
columns DOWN (reversed) per mbelib's `mbe_eccAmbe3600x2450Data`:
- ambe_d[0..12]  = ambe_fr[0][23..12] (u0)
- ambe_d[12..24] = ambe_fr[1][22..11] (u1, post-dewhitening)
- ambe_d[24..35] = ambe_fr[2][10..0]
- ambe_d[35..49] = ambe_fr[3][13..0]

No Golay error correction is performed in software.  Over UDP the
codeword bits are intact, and both Golay codes are systematic.  The
DV3000 chip does do FEC, and it's essential that we pass it the raw
on-air dibits so the FEC parity is correctly positioned -- passing a
deinterleaved codeword makes chip FEC fail and output silence.

#### Decode path

```
33-byte burst
  -> split around SYNC/EMB -> 3 x 36-dibit codewords
  -> pack raw dibits (pack_dibits) -> AmbeFrame [u8; 9]
  -> Vocoder::decode() (ThumbDV, AMBEserver, or mbelib)
     - hardware backends pass bytes through; chip deinterleaves + FEC
     - mbelib deinterleaves via rW/rX/rY/rZ, dewhitens row 1,
       extracts 49 source bits, hands to mbe_processAmbe2450Data
```

Every backend gets the same AmbeFrame.  mbelib is decode-only and may
be dropped once the hardware path is the only supported backend.

#### Frame type cycle

A voice superframe is: voice LC header, then repeating A-F voice bursts
(each carrying 3 AMBE codewords = 60 ms of audio), ending with a voice
terminator.  The DMRD flag byte carries frame_type + dtype_vseq so we can
track position without decoding the embedded signaling.

#### Test data

See [docs/TEST-VECTORS.md](docs/TEST-VECTORS.md) for the current coverage
matrix: ETSI normative tables (RS(12,9) generator, BPTC interleave), Python
reference implementations (dmrpy, ok-dmrlib), and live Brandmeister wire
captures (headers + terminators for 6 source radios).

### DMR Frame Assembly (ASL3 -> DMR)

Implemented in `dmr-wire/src/{bptc,embedded_lc,rs,fec,sync,frame}.rs`:

- `fec.rs` -- Golay(20,8,7), Hamming(13,9,3), Hamming(15,11,3),
  Hamming(16,11,4), QR(16,7,6) encoders.  Tables and parity equations
  cross-checked against DMRGateway.
- `rs.rs` -- RS(12,9) over GF(256) for Full LC checksum, with
  `LC_HEADER_MASK = 0x969696` and `LC_TERMINATOR_MASK = 0x999999`.
- `bptc.rs` -- BPTC(196,96) encoder for voice LC header/terminator.
  Builds the 9x15 matrix, applies row + column Hamming, interleaves
  via `(i * 181) % 196`, packs 33-byte burst (Info1 + SlotType1 +
  SYNC + SlotType2 + Info2).  Encoder validated byte-for-byte against
  11 live Brandmeister bursts from 6 different source radios.
- `embedded_lc.rs` -- BPTC(128,77) for the embedded LC fragments
  carried in voice bursts B-E.  LCSS sequencing: B=1 (first), C=3
  (continuation), D=3, E=2 (last), F=0 (null / RC).  Cross-checked
  against dmrpy's ETSI B.2 worked example.
- `sync.rs` -- SYNC patterns (`BS_VOICE_SYNC`, `BS_DATA_SYNC`) and
  the 48-bit EMB section builder (QR-encoded 7-bit info + 32-bit LC
  fragment).
- `frame.rs` -- voice burst assembly (three AMBE codewords +
  SYNC/EMB into 33 bytes) and the inverse disassembly for RX.

### PTT State Machine

Implemented in `dmr-wire/src/voice/ptt.rs` as `PttMachine`; the
`voice_task` event-loop dispatcher lives in `dmr-wire/src/voice.rs`.

States: `Idle` / `Rx(RxCall)` / `RxHang(deadline)` / `Tx(TxCall)`.
Half-duplex: RX and RxHang block TX; TX blocks RX.

**ASL3 -> DMR** (`on_usrp`):
- USRP `keyup=1` in Idle: create TxCall (new stream_id, precomputed
  embedded LC fragments), send voice LC header DMRD, buffer first PCM.
- In Tx: append PCM; every 3 frames, encode to AMBE, assemble burst
  with SYNC (burst A) or EMB-with-LC-fragment (bursts B-E) or null
  EMB (burst F), send DMRD.
- USRP `keyup=0` in Tx: flush partial burst with silence padding,
  send voice terminator DMRD, back to Idle.
- Ignored in Rx / RxHang.

**DMR -> ASL3** (`on_dmrd`):
- DMRD voice header on configured slot/TG/call_type: enter Rx state.
- DMRD voice (or implicit Rx start from Idle/RxHang): extract three
  AMBE codewords from the burst, decode each, emit USRP voice frames.
- DMRD terminator with matching stream_id: send USRP unkey, enter
  RxHang for configured `hang_time`.
- Ignored in Tx.

**Stream timeout** (`on_timeout`): if `stream_timeout` elapses in Rx
without new voice frames, send USRP unkey and enter RxHang.  Handles
lost terminators.

**Hang timer**: `RxHang(deadline)` delays returning to Idle so brief
pauses in speech don't drop PTT.  TX attempts during hang are ignored.

**TX timeout**: `tx_timeout` (default 180s) forces Tx -> Idle with
terminator if a call runs too long.

**Shutdown** (`on_shutdown`): on cancel, cleanly emit unkey (if Rx)
or terminator (if Tx) so the peer sees a graceful end.

`voice_task` is an infallible `async fn` that dispatches events
(on_dmrd / on_usrp / on_timeout / on_shutdown) to the PttMachine.
All per-frame errors (decode, send-drop) are logged and the loop
continues.

---

## Configuration Schema

TOML. See `config.example.toml`.

```toml
[repeater]
callsign = "N0CALL"
dmr_id = 1234567        # dedicated hotspot ID from radioid.net
# Optional RPTC fields (have sensible defaults if omitted):
# rx_freq, tx_freq, tx_power, color_code, latitude, longitude,
# height, location, description, url

[usrp]
local_host = "127.0.0.1"
local_port = 34001
remote_host = "127.0.0.1"
remote_port = 34002
# byte_swap = false    # enable for cross-endian USRP peer

[vocoder]
backend = "thumbdv"           # "thumbdv", "ambeserver", or "mbelib"
serial_port = "/dev/ttyUSB0"
# serial_baud = 460800
# host = "127.0.0.1"          # for ambeserver backend
# port = 2460

[dmr]
gateway = "both"              # "both", "dmr_to_fm", or "fm_to_dmr"
slot = 1                      # TS1 or TS2
talkgroup = 1
call_type = "group"
hang_time = "500ms"           # RX hang timer after terminator
stream_timeout = "5s"         # force RX unkey if voice stalls
tx_timeout = "180s"           # force TX terminator after this

[network]
profile = "brandmeister"
host = "master.example.net"
port = 62031
password = "your-hotspot-password"
keepalive_interval = "5s"
keepalive_missed_limit = 3
```

---

## Known Sharp Edges

**BM RPTC validation.** BM validates specific RPTC fields by format, not
whitelist.  See the RPTC Config Packet section above for the rules.

**BM reconnect lockout.** After a successful connection, BM keeps the hotspot
ID in "connected" state for some time. Immediate reconnection attempts are
rejected with MSTNAK until the state clears.

**DMR frame assembly** is the highest-risk remaining component. `dmr_data`
requires correct bit interleaving per ETSI TS 102 361-1. Wrong framing
produces garbled audio or silent discard at the network. Study HBlink3's
`dmr_utils.py` thoroughly before implementing.

**Voice superframe sequencing.** The A/B/C frame cycle must be correct and
the voice LC header must precede the first voice frame.

**DMR ID registration.** Register a dedicated hotspot ID at radioid.net. Do
not reuse an existing repeater or subscriber radio ID.

**AMBE vocoder mode.** For ThumbDV/AMBEserver, AMBE+2 for DMR requires
specific RATEP configuration (3600x2450). mbelib is decode-only.

**Vocoder is half-duplex, head-of-line.** One DV3000 chip serves both
directions.  `voice_task` takes the vocoder's `Arc<Mutex<...>>` lock
inside `spawn_blocking` for every encode/decode.  A stuck chip (or a
serial read that hits `SERIAL_TIMEOUT=2s`) blocks BOTH ingress
(DMR->FM) and egress (FM->DMR) for the duration.  Acceptable under
a well-behaved chip; if the chip wedges repeatedly, audio on both
paths drops.

**`try_send` drop warnings are problem sentinels, not noise.** The
logs in `dmr-wire/src/voice/ptt.rs` ("audio tx channel full,
dropping voice burst" and "DMRD out channel full, dropping packet")
should never fire in steady state -- the channels have 16-64 slots of
headroom and pacers drain promptly.  If either appears in
production, the consumer (usrp::tx_task or homebrew_client) is
stalled.  Investigate before dismissing.

The audio-tx side uses `mpsc::Sender::try_reserve_many` to reserve
all three voice-burst slots atomically: a partial burst would
produce an audible 20 ms gap mid-60 ms, so we drop the whole burst
with one warning when the channel can't take all three.

The DMRD-side warning specifically fires when `homebrew_client::run`
is in reconnect backoff (no consumer draining `dmrd_out_rx`); that
is by design -- voice frames queued during disconnect would be
discarded on reconnect anyway, so we drop them at the producer with
a log line rather than stall voice_task.

**Shutdown can wait up to SERIAL_TIMEOUT on an in-flight vocoder
call.** Cancel fires via `CancellationToken`; `spawn_blocking` keeps
running until its blocking read returns.  Tokio main's
`shutdown_timeout` caps the wait, but individual tasks may observe
2s latency before exit during an active call.

**Considered and rejected dependencies.**
- `hmac`: BM uses plain SHA256, not HMAC.
- `uuid`: stream IDs are u32, no UUIDs on the wire.
- `bytes`: `Vec<u8>` and `&[u8]` suffice.
