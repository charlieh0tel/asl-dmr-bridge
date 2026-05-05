//! Transport-agnostic audio frame for the seam between the DMR
//! voice task and any FM-side transport (USRP, simulated test rig,
//! future backends).  The DMR voice state machine consumes and emits
//! `AudioFrame`s; transport-specific framing (USRP seq, talkgroup,
//! FrameType, etc.) is the transport's responsibility.

/// 8 kHz mono PCM frame, 20 ms == 160 samples.  Same shape as
/// `ambe::PcmFrame`; the dmr-wire crate owns its own definition so
/// it does not need to re-export ambe constants in its API.
pub const VOICE_SAMPLES: usize = 160;

/// Audio events crossing the FM <-> DMR seam.  `keyup` carries the
/// PTT state; `samples` carries one frame of PCM when `keyup` is
/// true and there's audio for this slot, or `None` for keyup
/// transitions and unkey events that don't ship PCM.
///
/// `dmr_stream_id` is set only on the DMR -> FM direction and
/// carries the originating DMR stream_id so the FM transport can
/// correlate frames with calls.  Always `None` on the FM -> DMR
/// direction; PttMachine assigns the stream_id when it opens the
/// outgoing DMR call.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub keyup: bool,
    pub samples: Option<[i16; VOICE_SAMPLES]>,
    pub dmr_stream_id: Option<u32>,
}
