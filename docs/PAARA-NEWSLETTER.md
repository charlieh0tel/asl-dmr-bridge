# Linking W6OTX: How We Bridged FM and DMR Without DVSwitch

*by Christopher Hoover, AI6KG*

---

Key up an analog HT on 900 MHz and you can cleanly chat with someone
operating a digital DMR radio. If you have been active on the W6OTX
repeaters lately, you may have noticed something new: audio from our
33cm FM machine is now appearing on the UHF and VHF DMR repeaters, and
vice versa.

Hams familiar with digital linking might naturally assume the club is
running DVSwitch — the most common, go-to software suite for FM-to-DMR
bridges. We are not. While DVSwitch is a capable piece of software
that has served the community well, it is closed-source freeware. It
has no public bug tracker, no open repository, and no license
permitting modification or redistribution. For a club infrastructure
project, relying on a closed black box feels antithetical to the open,
experimental spirit of amateur radio.

Because I wanted something transparent, maintainable, and truly open
source, I decided to write an AllstarLink to DMR bridge from
scratch. Here is how the project works and how the pieces connect.


## Two Different Worlds

To understand why bridging these systems is a challenge, you have to
look at how different their underlying architectures are.

The W6OTX 33cm FM machine runs AllStarLink (ASL3), a peer-to-peer
network of Asterisk-based voice nodes speaking IAX2. (The machine also
accepts EchoLink connections.) ASL3 internally passes audio as raw,
uncompressed 8 kHz, 16-bit PCM. The bridge plugs in as a USRP endpoint
on the same machine.

On the flip side, the W6OTX UHF and VHF DMR repeaters live in a
different ecosystem. They connect to Brandmeister, one of the largest
global DMR networks for hams. Because our club repeaters are Motorola
machines, they connect to Brandmeister using IPSC (IP Site Connect).
The bridge takes a different path: Homebrew, a simpler UDP-based
protocol originally designed for personal hotspots.


## The Hidden Hurdles

In reality, digital-to-analog bridging introduces a set of less-obvious
engineering challenges beyond the obvious audio transport.

* **Call Tracking and Metadata:** DMR tags every transmission with
  source ID, talkgroup, slot, and color code; analog FM has none of
  that. The bridge tracks active calls on both sides and arbitrates
  access: while a DMR call is incoming, FM transmission is suppressed.

* **PTT and Timing State:** Managing push-to-talk between an untimed
  IP stream and a physical repeater requires careful synchronization.
  A configurable hang timer holds PTT open briefly after each
  transmission, absorbing the analog squelch tail so it does not
  kerchunk on the DMR side.

* **Gain Staging:** FM operators arrive at widely varying levels, and
  AMBE+2 is sensitive to input amplitude. The bridge applies
  configurable gain stages in both directions with per-call level
  tracking and a peak limiter.

But the single most significant technical bottleneck remains the voice
codec itself.

## The Elephant in the Room: Proprietary Voice Codec

DMR voice does not use open audio formats. It relies on a proprietary
codec called AMBE+2, developed by DVSI. The specification is not
public, and software implementations are controlled by copyright and
patents.

This is why most DIY FM-to-DMR setups require hardware: specifically,
the ThumbDV, a ~$120 USB dongle from Northwest Digital Radio
containing a licensed DVSI AMBE-3000R chip.  W6OTX currently uses a
ThumbDV for production traffic.

The project supports two dongle-free software alternatives, both
capable of running in real time on a Raspberry Pi 4:

* **Emulated MD380 Firmware:** The TYT MD380 handles AMBE+2 via a
  software implementation on its ARM processor; the bridge can run
  that exact firmware inside an ARM CPU emulator called dynarmic. No
  dongle required, but the firmware is proprietary and the legality of
  using it is unclear.

* **Neural AMBE (nambe):** An experimental AI6KG research project that
  trains neural networks for encoding and decoding using a hardware
  dongle as an oracle — no proprietary firmware involved. The decoder
  is already approaching hardware quality on typical bitstreams; the
  encoder is still being refined.

## Signal Flow

![Signal flow diagram](paara-newsletter-bridge-diagram.svg)

The diagram traces a U-shape: FM audio enters at top-left, flows
through the AllStarLink node into the bridge, down to Brandmeister, and back
out through the DMR repeaters at bottom-left. The dashed boxes show the other networks the 33cm machine serves in
addition to the bridge.
Digital voice travels either direction; end-to-end latency falls in
the same range as a VoIP phone call.

## Written in Rust

The bridge daemon is written in Rust, a systems language that compiles
to native code with performance comparable to C and C++. Beyond
performance, Rust enforces memory safety and catches data races at
compile time rather than at runtime. The multi-threaded daemon uses
the Tokio async runtime; audio frames flow between tasks over typed
channels, and if the encoder holds a buffer, the audio pipeline cannot
touch it simultaneously — the compiler rejects the attempt outright,
rather than letting it silently corrupt audio or crash at runtime.


## Open Source

The project is published on GitHub under the GPL at
https://github.com/charlieh0tel/asl-dmr-bridge.  The GPL means anyone
who modifies and distributes it must publish their changes under the
same terms — forks stay open.  Prebuilt Debian packages for Raspberry
Pi 4 are available on the releases page.  Anyone running ASL3 who
wants to experiment with a DMR link is welcome to try it.

On Brandmeister, the bridge is active on PAARA club talkgroup TG
3224295, reachable from any connected hotspot or repeater on the
network.

If you have questions or want to set up a link of your own, find me on
the W6OTX repeaters, by email, or on the club Discord.

*73 de AI6KG*
