# Linking W6OTX: How We Bridged FM and DMR Without DVSwitch

*by Christopher Hoover, AI6KG*

---

If you have been active on the W6OTX repeaters lately you may have noticed
something new: audio from the 33cm FM machine appearing on the UHF and VHF
DMR repeaters, and vice versa.  Hams familiar with digital linking might assume
the club is running DVSwitch -- the most common software for FM-to-DMR bridges.
We are not.  DVSwitch is closed-source freeware with no public bug tracker and
no license permitting modification or redistribution.  I wrote a replacement
from scratch, and this article explains how it works.

## Two Different Worlds

The W6OTX 33cm FM machine runs AllStarLink (ASL3), a peer-to-peer network of
Asterisk-based nodes that speak IAX2 directly to one another with no central
routing server.  The 33cm machine also accepts EchoLink connections.  ASL3
passes audio between nodes as raw PCM -- uncompressed 8 kHz 16-bit samples --
over a local protocol called USRP.  The bridge plugs into ASL3 as a USRP
endpoint: audio flows in and out over a UDP port on the same machine.

The W6OTX UHF and VHF DMR repeaters connect to Brandmeister, one of the major
global DMR network operators, using IPSC (IP Site Connect) -- the commercial
protocol built into Motorola and Hytera infrastructure.  The bridge connects to
Brandmeister a different way: via Homebrew, a simpler UDP protocol designed for
personal hotspots.

Connecting the two worlds is conceptually straightforward: receive PCM from
ASL3, encode it into DMR voice frames, and transmit via Homebrew.  In the
other direction, decode incoming DMR frames and push PCM back to ASL3.  The
difficulty is entirely in the codec step.

## The AMBE+2 Problem

DMR voice uses a codec called AMBE+2, developed by DVSI.  The specification is
not public and software implementations are tightly controlled.  This is why
most FM-to-DMR bridges rely on the ThumbDV, a USB dongle from Northwest Digital
Radio (around $100) containing a licensed DVSI AMBE-3000R chip that handles
encoding and decoding in hardware.  W6OTX currently uses a ThumbDV.

The bridge also supports two dongle-free software alternatives, both capable of
running in real time on a Raspberry Pi 4:

**Emulated MD380 firmware**: The TYT MD380 is a consumer DMR handheld that runs
the AMBE+2 codec on its own ARM processor.  Researchers have documented the
MD380 firmware in detail.  This backend runs that firmware inside an ARM CPU
emulator called dynarmic.  No dongle is required, but the firmware is
proprietary and the legal picture around extraction varies by jurisdiction.

**nambe**: A research project developed alongside the bridge, still a work in
progress.  Rather than emulating proprietary code, nambe trains small neural
networks to perform AMBE+2 encoding and decoding, using a hardware chip to
generate training data.  It runs in entirely original code with no dongle and
no proprietary firmware.  The decoder is approaching chip quality on typical
voice; the encoder is still being refined.

## Signal Flow

![Signal flow diagram](bridge-diagram.svg)

## Written in Rust

The bridge is written in Rust, a systems language that compiles to native code
with performance comparable to C.  Rust catches memory safety bugs and data
races at compile time -- properties that matter for a daemon running around the
clock.

## Open Source

The project is published on GitHub under the GPL at
https://github.com/charlieh0tel/asl-dmr-bridge.  Prebuilt Debian packages for
Raspberry Pi 4 are available on the releases page.  Anyone running ASL3 who
wants to experiment with a DMR link is welcome to try it.

On Brandmeister the bridge is active on PAARA club talkgroup TG 3224295,
reachable from any connected hotspot or repeater on the network.

If you have questions or want to set up a link of your own, find me on the
W6OTX repeaters or by email.

*73 de AI6KG*
