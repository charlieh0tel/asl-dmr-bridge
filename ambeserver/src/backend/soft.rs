//! In-process software vocoder.  Parses each packet, fabricates the
//! chip's responses for control packets, and runs encode/decode
//! through `ambe::Vocoder`.  Refuses any non-DMR RATEP and poisons
//! the session until the next RESET so a misconfigured client can't
//! keep feeding data the vocoder would mis-decode.

use ambe::Vocoder;
use anyhow::Result;
use dsp::dB;
use dv3000_wire::CONTROL_GAIN;
use dv3000_wire::CONTROL_PRODID;
use dv3000_wire::CONTROL_RATEP;
use dv3000_wire::CONTROL_RESET;
use dv3000_wire::Packet;
use dv3000_wire::build_ambe;
use dv3000_wire::build_audio;
use dv3000_wire::build_control_ack;
use dv3000_wire::build_prodid_response;
use dv3000_wire::build_ready;
use dv3000_wire::parse;
use dv3000_wire::rates::RATEP_DMR;
use tracing::warn;

use super::Backend;

/// Result code byte the chip returns in its short Control acks.
const ACK_OK: u8 = 0x00;
/// Non-zero result code in the chip's short ack format means
/// "request rejected".  Real chip values are vendor-specific and
/// undocumented; pick a single canonical sentinel.
const ACK_REJECT: u8 = 0x01;

pub(crate) struct SoftBackend {
    vocoder: Box<dyn Vocoder>,
    /// NUL-terminated product string returned for PRODID queries
    /// (`"AMBE3000R / dynarmic\0"` or `/ neural`).  Truthful so
    /// hexdump-debugging operators can tell what they're talking
    /// to.
    prodid: Vec<u8>,
    /// `true` after a RATEP DMR has been ack'd; voice traffic
    /// before this is dropped to keep the vocoder from running
    /// against a wrong rate the client thinks it negotiated.
    ratep_acked: bool,
    /// Set when the client sent a non-DMR RATEP.  All subsequent
    /// traffic from this session is dropped until the next RESET
    /// clears the flag.
    poisoned: bool,
}

impl SoftBackend {
    pub(crate) fn new(vocoder: Box<dyn Vocoder>, backend_label: &str) -> Self {
        let mut prodid = Vec::with_capacity(32);
        prodid.extend_from_slice(b"AMBE3000R / ");
        prodid.extend_from_slice(backend_label.as_bytes());
        prodid.push(0);
        Self {
            vocoder,
            prodid,
            ratep_acked: false,
            poisoned: false,
        }
    }
}

impl Backend for SoftBackend {
    fn handle(&mut self, request: &[u8]) -> Result<Option<Vec<u8>>> {
        let (packet, _) = match parse(request) {
            Ok(p) => p,
            Err(e) => {
                warn!("parse error: {e}; dropping");
                return Ok(None);
            }
        };

        // RESET always honored, even on a poisoned session: it's
        // the client's way of saying "let's start over".
        if let Packet::Control { field_id, .. } = &packet
            && *field_id == CONTROL_RESET
        {
            self.ratep_acked = false;
            self.poisoned = false;
            self.vocoder.reset();
            return Ok(Some(build_ready()));
        }

        if self.poisoned {
            return Ok(None);
        }

        match packet {
            Packet::Control { field_id, data } => match field_id {
                CONTROL_RATEP => {
                    if data.len() == RATEP_DMR.len() && data == RATEP_DMR {
                        self.ratep_acked = true;
                        Ok(Some(build_control_ack(CONTROL_RATEP, ACK_OK)))
                    } else {
                        warn!("non-DMR RATEP rejected; session poisoned until RESET");
                        self.poisoned = true;
                        Ok(Some(build_control_ack(CONTROL_RATEP, ACK_REJECT)))
                    }
                }
                CONTROL_GAIN => {
                    if data.len() < 2 {
                        warn!("malformed GAIN payload (len={}); dropping", data.len());
                        return Ok(None);
                    }
                    let in_db = data[0] as i8;
                    let out_db = data[1] as i8;
                    self.vocoder
                        .set_gain(dB(f32::from(in_db)), dB(f32::from(out_db)))?;
                    Ok(Some(build_control_ack(CONTROL_GAIN, ACK_OK)))
                }
                CONTROL_PRODID => Ok(Some(build_prodid_response(&self.prodid))),
                _ => Ok(None),
            },
            Packet::Audio(pcm) => {
                if !self.ratep_acked {
                    warn!("audio packet before RATEP DMR ack; dropping");
                    return Ok(None);
                }
                let ambe = self.vocoder.encode(&pcm)?;
                Ok(Some(build_ambe(&ambe)))
            }
            Packet::Ambe(frame) => {
                if !self.ratep_acked {
                    warn!("ambe packet before RATEP DMR ack; dropping");
                    return Ok(None);
                }
                let pcm = self.vocoder.decode(Some(&frame))?;
                Ok(Some(build_audio(&pcm)))
            }
            Packet::AmbeBits { bits, .. } => {
                warn!("non-DMR AMBE ({bits} bits) in software mode; dropping");
                Ok(None)
            }
        }
    }

    fn on_takeover(&mut self) {
        // New peer; drop the prior peer's RATEP / poison state so they
        // negotiate from scratch (same effect as a fresh RESET).
        self.vocoder.reset();
        self.ratep_acked = false;
        self.poisoned = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ambe::VocoderError;
    use dmr_types::AmbeFrame;
    use dmr_types::PcmFrame;
    use dv3000_wire::CONTROL_READY;
    use dv3000_wire::build_audio;
    use dv3000_wire::build_gain;
    use dv3000_wire::build_prodid;
    use dv3000_wire::build_ratep_custom;
    use dv3000_wire::build_ratep_dmr;
    use dv3000_wire::build_reset;
    use dv3000_wire::is_ratep_ack;
    use dv3000_wire::is_ready;
    use dv3000_wire::rates::RATEP_RAW;

    /// Returns deterministic frames so the round-trip tests can
    /// assert without depending on a real codec.
    struct FakeVocoder;
    impl Vocoder for FakeVocoder {
        fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
            Ok([0xAB; 9])
        }
        fn decode(&mut self, _ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
            Ok([1234i16; 160])
        }
        fn reset(&mut self) {}
        fn set_gain(&mut self, _: dB, _: dB) -> Result<(), VocoderError> {
            Ok(())
        }
    }

    fn fresh() -> SoftBackend {
        SoftBackend::new(Box::new(FakeVocoder), "fake")
    }

    fn parse_response(bytes: &[u8]) -> Packet {
        parse(bytes).expect("parse").0
    }

    #[test]
    fn reset_emits_ready() {
        let mut b = fresh();
        let resp = b.handle(&build_reset()).unwrap().expect("response");
        assert!(is_ready(&parse_response(&resp)));
    }

    #[test]
    fn ratep_dmr_acked() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        let resp = b.handle(&build_ratep_dmr()).unwrap().expect("response");
        assert!(is_ratep_ack(&parse_response(&resp)));
        // Short-form ack: header(4) + field_id(1) + result(1).
        assert_eq!(resp.len(), 6);
        assert_eq!(resp[5], ACK_OK);
    }

    #[test]
    fn on_takeover_clears_ratep_and_poison() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        b.handle(&build_ratep_dmr()).unwrap();
        assert!(b.ratep_acked);
        b.handle(&build_ratep_custom(&RATEP_RAW)).unwrap();
        assert!(b.poisoned);

        Backend::on_takeover(&mut b);
        assert!(!b.ratep_acked);
        assert!(!b.poisoned);
    }

    #[test]
    fn non_dmr_ratep_rejected_and_poisons() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        let resp = b
            .handle(&build_ratep_custom(&RATEP_RAW))
            .unwrap()
            .expect("response");
        assert_eq!(resp[5], ACK_REJECT);
        assert!(b.poisoned);
        // Subsequent voice packet on a poisoned session: drop.
        let pcm = [0i16; 160];
        assert!(b.handle(&build_audio(&pcm)).unwrap().is_none());
        // RESET clears the poison.
        b.handle(&build_reset()).unwrap();
        assert!(!b.poisoned);
    }

    #[test]
    fn voice_before_ratep_dropped() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        let pcm = [0i16; 160];
        assert!(b.handle(&build_audio(&pcm)).unwrap().is_none());
    }

    #[test]
    fn audio_round_trip_after_ratep() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        b.handle(&build_ratep_dmr()).unwrap();
        let pcm = [0i16; 160];
        let resp = b.handle(&build_audio(&pcm)).unwrap().expect("response");
        match parse_response(&resp) {
            Packet::Ambe(frame) => assert_eq!(frame, [0xAB; 9]),
            other => panic!("expected Ambe, got {other:?}"),
        }
    }

    #[test]
    fn gain_acked() {
        let mut b = fresh();
        b.handle(&build_reset()).unwrap();
        let resp = b.handle(&build_gain(-3, 6)).unwrap().expect("response");
        let p = parse_response(&resp);
        assert!(matches!(p, Packet::Control { field_id, .. } if field_id == CONTROL_GAIN));
        assert_eq!(resp[5], ACK_OK);
    }

    #[test]
    fn prodid_returns_backend_label() {
        let mut b = fresh();
        let resp = b.handle(&build_prodid()).unwrap().expect("response");
        match parse_response(&resp) {
            Packet::Control { field_id, data } => {
                assert_eq!(field_id, CONTROL_PRODID);
                let s = std::str::from_utf8(&data).expect("utf8");
                assert!(
                    s.starts_with("AMBE3000R / fake"),
                    "expected AMBE3000R / fake prefix, got {s:?}"
                );
            }
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn ready_field_id_constant_match() {
        // Sanity check that build_ready actually uses CONTROL_READY,
        // not CONTROL_RESET (they're easy to swap accidentally).
        let mut b = fresh();
        let resp = b.handle(&build_reset()).unwrap().expect("response");
        match parse_response(&resp) {
            Packet::Control { field_id, .. } => assert_eq!(field_id, CONTROL_READY),
            other => panic!("expected Control, got {other:?}"),
        }
    }
}
