//! DMR network client.
//!
//! Implements auth (RPTL/RPTK) and keepalive (RPTPING/MSTPONG) per the
//! BM Homebrew protocol, with RPTC config packet.  Auto-reconnects with
//! exponential backoff on network errors.  Sends RPTCL on graceful
//! shutdown.
//!
//! References:
//!   https://wiki.brandmeister.network/index.php/Homebrew/example/php2
//!   https://wiki.brandmeister.network/index.php/Homebrew_repeater_protocol/Spec

use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use secrecy::SecretString;
use sha2::Digest;
use sha2::Sha256;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::interval;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::info;
use tracing::warn;

use crate::config::RuntimeConfig;
use crate::network::NetworkProfile;
use dmr_types::DmrId;
use dmr_types::REPEATER_ID_WIRE_LEN;
use dmr_wire::dmrd::Dmrd;
use dmr_wire::voice::ControlEvent;

#[derive(Debug, thiserror::Error)]
pub(crate) enum NetworkError {
    #[error("resolving {host}:{port}")]
    Resolve {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },

    #[error("no addresses resolved for {host}:{port}")]
    NoAddress { host: String, port: u16 },

    #[error("UDP I/O")]
    Io(#[from] std::io::Error),

    #[error("auth cancelled")]
    AuthCancelled,

    #[error("auth timed out after {0:?}")]
    AuthTimeout(Duration),

    #[error("master rejected {stage} (MSTNAK)")]
    MasterNak { stage: &'static str },

    #[error("master sent MSTCL (disconnect)")]
    MasterDisconnect,

    #[error("unexpected response to {stage}: {preview:?}")]
    UnexpectedResponse {
        stage: &'static str,
        preview: Vec<u8>,
    },

    #[error("lost connection: {0} missed keepalives")]
    KeepaliveLost(u32),
}

/// Protocol tags.
const TAG_RPTL: &[u8] = b"RPTL";
const TAG_RPTK: &[u8] = b"RPTK";
const TAG_RPTPING: &[u8] = b"RPTPING";
const TAG_RPTCL: &[u8] = b"RPTCL";

/// Response tags.
const TAG_RPTACK: &[u8] = b"RPTACK";
const TAG_MSTNAK: &[u8] = b"MSTNAK";
const TAG_MSTPONG: &[u8] = b"MSTPONG";
const TAG_MSTCL: &[u8] = b"MSTCL";
const TAG_DMRD: &[u8] = b"DMRD";
const TAG_RPTSBKN: &[u8] = b"RPTSBKN";

const NONCE_LEN: usize = 4;
const DIGEST_LEN: usize = 32;
const RPTK_LEN: usize = TAG_RPTK.len() + REPEATER_ID_WIRE_LEN + DIGEST_LEN;

const MAX_RECV: usize = 512;
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

/// Reconnect backoff: doubles each failure, capped.
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Exponential reconnect-delay state machine.  `current()` is the
/// next sleep duration; `double()` advances after the failure;
/// `reset()` returns to the initial value (called when the prior
/// session reached `authed` so a transient drop doesn't drag the
/// backoff up to its cap on healthy peers).
#[derive(Debug)]
pub(crate) struct ReconnectBackoff {
    current: Duration,
}

impl ReconnectBackoff {
    pub(crate) fn new() -> Self {
        Self {
            current: BACKOFF_INITIAL,
        }
    }

    pub(crate) fn current(&self) -> Duration {
        self.current
    }

    pub(crate) fn double(&mut self) {
        self.current = (self.current * 2).min(BACKOFF_MAX);
    }

    pub(crate) fn reset(&mut self) {
        self.current = BACKOFF_INITIAL;
    }
}

/// Classified inbound packet in the keepalive loop.  Carves the
/// side-effect-free "what is this packet?" decision out of the
/// handler so it can be unit-tested with synthetic byte strings
/// instead of a live socket.
#[derive(Debug)]
pub(crate) enum InboundEvent {
    Pong,
    Dmrd(Dmrd),
    DmrdParseError(dmr_wire::dmrd::DmrdError),
    MasterNak,
    MasterDisconnect,
    RptAck,
    RptSbkn,
    Unknown,
}

pub(crate) fn classify(data: &[u8]) -> InboundEvent {
    if data.starts_with(TAG_MSTPONG) {
        InboundEvent::Pong
    } else if data.starts_with(TAG_DMRD) {
        match Dmrd::parse(data) {
            Ok(pkt) => InboundEvent::Dmrd(pkt),
            Err(e) => InboundEvent::DmrdParseError(e),
        }
    } else if data.starts_with(TAG_MSTNAK) {
        InboundEvent::MasterNak
    } else if data.starts_with(TAG_MSTCL) {
        InboundEvent::MasterDisconnect
    } else if data.starts_with(TAG_RPTACK) {
        InboundEvent::RptAck
    } else if data.starts_with(TAG_RPTSBKN) {
        InboundEvent::RptSbkn
    } else {
        InboundEvent::Unknown
    }
}

/// Per-connection keepalive state: tracks whether each ping was
/// answered before the next tick fires, and escalates to
/// `KeepaliveLost` after `missed_limit` consecutive misses.
/// Extracted so the missed-pong logic is unit-testable without
/// spinning up a socket + real timer.
pub(crate) struct KeepaliveTracker {
    last_ping: Option<Instant>,
    last_pong: Instant,
    missed: u32,
    missed_limit: u32,
}

impl KeepaliveTracker {
    pub(crate) fn new(missed_limit: u32) -> Self {
        Self {
            last_ping: None,
            last_pong: Instant::now(),
            missed: 0,
            missed_limit,
        }
    }

    /// Call when a tick fires.  Returns Err if we've hit the missed-
    /// pong limit; Ok otherwise.  The caller then sends the next
    /// ping and passes `Instant::now()` to `record_ping`.
    pub(crate) fn check_missed(&mut self) -> Result<(), NetworkError> {
        if let Some(prev) = self.last_ping
            && self.last_pong < prev
        {
            self.missed += 1;
            if self.missed >= self.missed_limit {
                return Err(NetworkError::KeepaliveLost(self.missed));
            }
        }
        Ok(())
    }

    pub(crate) fn record_ping(&mut self, at: Instant) {
        self.last_ping = Some(at);
    }

    pub(crate) fn record_pong(&mut self, at: Instant) {
        self.last_pong = at;
        self.missed = 0;
    }

    pub(crate) fn missed(&self) -> u32 {
        self.missed
    }
}

/// Build a tag + repeater_id packet.
fn build_tagged(tag: &[u8], dmr_id: DmrId) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(tag.len() + REPEATER_ID_WIRE_LEN);
    pkt.extend_from_slice(tag);
    pkt.extend_from_slice(&dmr_id.to_be_bytes());
    pkt
}

fn build_rptk(dmr_id: DmrId, nonce: &[u8], password: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();

    let mut pkt = Vec::with_capacity(RPTK_LEN);
    pkt.extend_from_slice(TAG_RPTK);
    pkt.extend_from_slice(&dmr_id.to_be_bytes());
    pkt.extend_from_slice(&digest);
    pkt
}

/// Best-effort disconnect. Ignores errors.
async fn send_rptcl(socket: &UdpSocket, dmr_id: DmrId) {
    let _ = socket.send(&build_tagged(TAG_RPTCL, dmr_id)).await;
    info!("sent RPTCL disconnect");
}

/// Run the DMR network client with auto-reconnect.
///
/// Returns Ok(()) on graceful shutdown (token cancelled).
#[expect(
    clippy::too_many_arguments,
    reason = "run wires the Homebrew session to inbound DMR, bounded voice out, unbounded control out, session-control notifications, and cancellation."
)]
pub(crate) async fn run(
    config: &RuntimeConfig,
    password: &SecretString,
    profile: &dyn NetworkProfile,
    dmrd_tx: mpsc::Sender<Dmrd>,
    mut dmrd_voice_out_rx: mpsc::Receiver<Vec<u8>>,
    mut dmrd_ctl_out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    control_tx: mpsc::Sender<ControlEvent>,
    cancel: CancellationToken,
) -> Result<(), NetworkError> {
    let mut backoff = ReconnectBackoff::new();

    loop {
        match connect_once(
            config,
            password,
            profile,
            dmrd_tx.clone(),
            &mut dmrd_voice_out_rx,
            &mut dmrd_ctl_out_rx,
            &control_tx,
            cancel.clone(),
        )
        .await
        {
            Ok(_) => return Ok(()),
            Err(_) if cancel.is_cancelled() => return Ok(()),
            Err(ConnectError { authed, source }) => {
                if authed {
                    backoff.reset();
                }
                warn!(
                    "connection error: {source}; reconnecting in {:?}",
                    backoff.current()
                );
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    _ = sleep(backoff.current()) => {}
                }
                backoff.double();
            }
        }
    }
}

/// Error from a single connection attempt.  `authed` is true if auth
/// succeeded at least once before the failure (used to reset backoff
/// on transient drops after a healthy session).
struct ConnectError {
    authed: bool,
    source: NetworkError,
}

impl From<NetworkError> for ConnectError {
    fn from(source: NetworkError) -> Self {
        Self {
            authed: false,
            source,
        }
    }
}

/// Do one full auth+keepalive cycle.  Returns Ok(()) only on graceful
/// cancel; any network-level issue returns Err.
#[expect(
    clippy::too_many_arguments,
    reason = "connect_once carries the full per-session wiring so reconnect preserves ownership of both outbound queues and the control channel."
)]
async fn connect_once(
    config: &RuntimeConfig,
    password: &SecretString,
    profile: &dyn NetworkProfile,
    dmrd_tx: mpsc::Sender<Dmrd>,
    dmrd_voice_out_rx: &mut mpsc::Receiver<Vec<u8>>,
    dmrd_ctl_out_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    control_tx: &mpsc::Sender<ControlEvent>,
    cancel: CancellationToken,
) -> Result<(), ConnectError> {
    let host = &config.network.host;
    let port = config.network.port;

    // DNS lookup + bind + connect, cancel-aware.  Without this wrap,
    // a stalled DNS resolver could delay shutdown by up to the OS
    // resolver timeout (often 30 s).  authenticate() is separately
    // cancel-aware via its own select; keepalive_loop handles cancel
    // internally; so only the pre-auth setup needed this wrapper.
    let setup = async {
        let addr = tokio::net::lookup_host(format!("{host}:{port}"))
            .await
            .map_err(|source| NetworkError::Resolve {
                host: host.clone(),
                port,
                source,
            })?
            .next()
            .ok_or_else(|| NetworkError::NoAddress {
                host: host.clone(),
                port,
            })?;
        let socket = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(NetworkError::Io)?,
        );
        socket.connect(addr).await.map_err(NetworkError::Io)?;
        Ok::<_, NetworkError>((addr, socket))
    };
    let (addr, socket) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(NetworkError::AuthCancelled.into()),
        result = setup => result?,
    };
    info!("connecting to master at {addr}");

    authenticate(&socket, config, password, profile, &cancel).await?;
    info!("authenticated with master");

    // Drain stale outbound DMRD packets from a prior session.
    while dmrd_voice_out_rx.try_recv().is_ok() {}
    while dmrd_ctl_out_rx.try_recv().is_ok() {}
    if control_tx.send(ControlEvent::NetworkReset).await.is_err() {
        return Ok(());
    }

    let result = keepalive_loop(
        &socket,
        config,
        &dmrd_tx,
        dmrd_voice_out_rx,
        dmrd_ctl_out_rx,
        cancel.clone(),
    )
    .await;

    // Best-effort RPTCL on any disconnect (clean or error).
    send_rptcl(&socket, config.repeater.dmr_id).await;

    result.map_err(|source| ConnectError {
        authed: true,
        source,
    })
}

async fn authenticate(
    socket: &UdpSocket,
    config: &RuntimeConfig,
    password: &SecretString,
    profile: &dyn NetworkProfile,
    cancel: &CancellationToken,
) -> Result<(), NetworkError> {
    // Overall deadline covers the whole auth sequence so a server sending
    // stale/irrelevant packets cannot keep resetting per-recv timeouts.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(NetworkError::AuthCancelled),
        result = tokio::time::timeout(
            AUTH_TIMEOUT,
            authenticate_inner(socket, config, password, profile),
        ) => {
            match result {
                Ok(inner) => inner,
                Err(_) => Err(NetworkError::AuthTimeout(AUTH_TIMEOUT)),
            }
        }
    }
}

async fn authenticate_inner(
    socket: &UdpSocket,
    config: &RuntimeConfig,
    password: &SecretString,
    profile: &dyn NetworkProfile,
) -> Result<(), NetworkError> {
    let mut buf = [0u8; MAX_RECV];
    let dmr_id = config.repeater.dmr_id;

    // Step 1: RPTL
    socket.send(&build_tagged(TAG_RPTL, dmr_id)).await?;
    debug!("sent RPTL");

    let nonce = loop {
        let len = socket.recv(&mut buf).await?;
        let data = &buf[..len];
        if data.starts_with(TAG_MSTNAK) {
            return Err(NetworkError::MasterNak { stage: "login" });
        }
        if data.starts_with(TAG_RPTACK) && data.len() >= TAG_RPTACK.len() + NONCE_LEN {
            let mut nonce = [0u8; NONCE_LEN];
            nonce.copy_from_slice(&data[TAG_RPTACK.len()..TAG_RPTACK.len() + NONCE_LEN]);
            break nonce;
        }
    };

    // Step 2: RPTK
    socket
        .send(&build_rptk(dmr_id, &nonce, password.expose_secret()))
        .await?;
    debug!("sent RPTK");

    let len = socket.recv(&mut buf).await?;
    let data = &buf[..len];
    if data.starts_with(TAG_MSTNAK) {
        return Err(NetworkError::MasterNak {
            stage: "auth (incorrect password)",
        });
    }
    if !data.starts_with(TAG_RPTACK) {
        return Err(NetworkError::UnexpectedResponse {
            stage: "RPTK",
            preview: data[..data.len().min(10)].to_vec(),
        });
    }

    // Step 3: RPTC config
    let config_pkt = profile.config_packet(config);
    debug!("sending RPTC config: {} bytes", config_pkt.len());
    socket.send(&config_pkt).await?;

    let len = socket.recv(&mut buf).await?;
    let data = &buf[..len];
    if data.starts_with(TAG_MSTNAK) {
        return Err(NetworkError::MasterNak { stage: "config" });
    }
    if !data.starts_with(TAG_RPTACK) {
        return Err(NetworkError::UnexpectedResponse {
            stage: "RPTC",
            preview: data[..data.len().min(10)].to_vec(),
        });
    }
    debug!("RPTC accepted");

    Ok(())
}

async fn keepalive_loop(
    socket: &UdpSocket,
    config: &RuntimeConfig,
    dmrd_tx: &mpsc::Sender<Dmrd>,
    dmrd_voice_out_rx: &mut mpsc::Receiver<Vec<u8>>,
    dmrd_ctl_out_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    cancel: CancellationToken,
) -> Result<(), NetworkError> {
    let mut buf = [0u8; MAX_RECV];
    let ping = build_tagged(TAG_RPTPING, config.repeater.dmr_id);
    let mut ticker = interval(config.network.keepalive_interval);
    // If the select loop stalls (e.g. a slow socket.send), the default
    // MissedTickBehavior::Burst would fire catch-up pings back-to-back
    // once we resumed.  Delay re-anchors the next tick to now + period
    // instead, which is what we want for a heartbeat.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tracker = KeepaliveTracker::new(config.network.keepalive_missed_limit);

    // Skip first immediate tick.
    ticker.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = ticker.tick() => {
                if let Err(e) = tracker.check_missed() {
                    warn!(
                        "missed pong ({}/{})",
                        tracker.missed(), config.network.keepalive_missed_limit,
                    );
                    return Err(e);
                }
                if tracker.missed() > 0 {
                    warn!(
                        "missed pong ({}/{})",
                        tracker.missed(), config.network.keepalive_missed_limit,
                    );
                }
                socket.send(&ping).await?;
                tracker.record_ping(Instant::now());
                debug!("sent RPTPING");
            }
            result = socket.recv(&mut buf) => {
                let len = result?;
                let data = &buf[..len];
                match classify(data) {
                    InboundEvent::Pong => {
                        tracker.record_pong(Instant::now());
                        debug!("got MSTPONG");
                    }
                    InboundEvent::Dmrd(pkt) => {
                        debug!(
                            seq = pkt.seq,
                            src_id = pkt.src_id,
                            dst_id = pkt.dst_id,
                            stream_id = pkt.stream_id,
                            slot = ?pkt.slot,
                            frame_type = ?pkt.frame_type,
                            dtype_vseq = pkt.dtype_vseq,
                            "DMRD rx"
                        );
                        if dmrd_tx.send(pkt).await.is_err() {
                            return Ok(());
                        }
                    }
                    InboundEvent::DmrdParseError(e) => {
                        warn!("DMRD parse error: {e}");
                    }
                    InboundEvent::MasterNak => {
                        return Err(NetworkError::MasterNak { stage: "keepalive" });
                    }
                    InboundEvent::MasterDisconnect => {
                        return Err(NetworkError::MasterDisconnect);
                    }
                    InboundEvent::RptAck => debug!("got RPTACK"),
                    InboundEvent::RptSbkn => debug!("got RPTSBKN (beacon request)"),
                    InboundEvent::Unknown => {
                        warn!(
                            "unknown packet ({len} bytes): {:?}",
                            String::from_utf8_lossy(&data[..data.len().min(8)])
                        );
                    }
                }
            }
            pkt = dmrd_ctl_out_rx.recv() => {
                let Some(pkt) = pkt else { return Ok(()) };
                if let Err(e) = socket.send(&pkt).await {
                    warn!("DMRD tx control error ({} bytes): {e}", pkt.len());
                } else {
                    debug!("DMRD tx control ({} bytes)", pkt.len());
                }
            }
            pkt = dmrd_voice_out_rx.recv() => {
                let Some(pkt) = pkt else { return Ok(()) };
                // Log-and-continue on send errors rather than tear
                // down the session.  Most UDP send errors here are
                // transient (route flap, kernel buffer full); the
                // keepalive tracker is the authoritative signal for
                // a genuinely dead session.  Dropping one packet is
                // strictly better than triggering a reconnect (and
                // its backoff sleep) for every late-network voice
                // burst.
                if let Err(e) = socket.send(&pkt).await {
                    warn!("DMRD tx voice error ({} bytes): {e}", pkt.len());
                } else {
                    debug!("DMRD tx voice ({} bytes)", pkt.len());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ID_BYTES: [u8; 4] = [0x00, 0x12, 0xD6, 0x87];

    fn test_dmr_id() -> DmrId {
        DmrId::try_from(1234567).unwrap()
    }

    #[test]
    fn rptl_format() {
        let pkt = build_tagged(TAG_RPTL, test_dmr_id());
        assert_eq!(&pkt[..TAG_RPTL.len()], TAG_RPTL);
        assert_eq!(&pkt[TAG_RPTL.len()..], &TEST_ID_BYTES);
    }

    #[test]
    fn rptk_format() {
        let nonce = [0x01, 0x02, 0x03, 0x04];
        let pkt = build_rptk(test_dmr_id(), &nonce, "secret");
        assert_eq!(pkt.len(), RPTK_LEN);
        assert_eq!(&pkt[..TAG_RPTK.len()], TAG_RPTK);

        let mut hasher = Sha256::new();
        hasher.update(nonce);
        hasher.update(b"secret");
        let expected = hasher.finalize();
        assert_eq!(&pkt[TAG_RPTK.len() + REPEATER_ID_WIRE_LEN..], &expected[..]);
    }

    #[test]
    fn rptping_format() {
        let pkt = build_tagged(TAG_RPTPING, test_dmr_id());
        assert_eq!(&pkt[..TAG_RPTPING.len()], TAG_RPTPING);
        assert_eq!(&pkt[TAG_RPTPING.len()..], &TEST_ID_BYTES);
    }

    #[test]
    fn rptcl_format() {
        let pkt = build_tagged(TAG_RPTCL, test_dmr_id());
        assert_eq!(&pkt[..TAG_RPTCL.len()], TAG_RPTCL);
        assert_eq!(&pkt[TAG_RPTCL.len()..], &TEST_ID_BYTES);
    }

    // --- classify ---

    #[test]
    fn classify_pong() {
        assert!(matches!(classify(b"MSTPONG"), InboundEvent::Pong));
        assert!(matches!(
            classify(b"MSTPONG\x00\x12\xD6\x87"),
            InboundEvent::Pong
        ));
    }

    #[test]
    fn classify_mstnak() {
        assert!(matches!(classify(b"MSTNAK"), InboundEvent::MasterNak));
    }

    #[test]
    fn classify_mstcl() {
        assert!(matches!(classify(b"MSTCL"), InboundEvent::MasterDisconnect));
    }

    #[test]
    fn classify_rptack() {
        assert!(matches!(
            classify(b"RPTACK\x01\x02\x03\x04"),
            InboundEvent::RptAck
        ));
    }

    #[test]
    fn classify_rptsbkn() {
        assert!(matches!(classify(b"RPTSBKN"), InboundEvent::RptSbkn));
    }

    #[test]
    fn classify_unknown() {
        assert!(matches!(classify(b""), InboundEvent::Unknown));
        assert!(matches!(classify(b"GARBAGE"), InboundEvent::Unknown));
    }

    #[test]
    fn classify_dmrd_valid() {
        // Minimal 53-byte DMRD: 4-byte tag + 49-byte payload.
        let mut pkt = Vec::with_capacity(53);
        pkt.extend_from_slice(b"DMRD");
        pkt.push(0); // seq
        pkt.extend_from_slice(&[0, 0, 1]); // src_id
        pkt.extend_from_slice(&[0, 0, 9]); // dst_id
        pkt.extend_from_slice(&[0, 0, 0, 2]); // repeater_id
        pkt.push(0); // flags
        pkt.extend_from_slice(&[0, 0, 0, 0]); // stream_id
        pkt.extend_from_slice(&[0u8; 33]); // dmr_data
        assert!(matches!(classify(&pkt), InboundEvent::Dmrd(_)));
    }

    #[test]
    fn classify_dmrd_too_short_is_parse_error() {
        // Correct tag but truncated body -> DmrdError::TooShort.
        assert!(matches!(
            classify(b"DMRD\x00\x01\x02"),
            InboundEvent::DmrdParseError(_)
        ));
    }

    // --- KeepaliveTracker ---

    #[test]
    fn tracker_first_tick_does_not_count_as_miss() {
        // Before any ping has been sent, check_missed is a no-op;
        // catches the first-iteration false-positive the inline
        // code's Option<last_ping> guarded against.
        let mut t = KeepaliveTracker::new(3);
        t.check_missed().expect("first tick should not miss");
        assert_eq!(t.missed(), 0);
    }

    #[test]
    fn tracker_ping_then_pong_no_miss() {
        let mut t = KeepaliveTracker::new(3);
        let t0 = Instant::now();
        t.record_ping(t0);
        t.record_pong(t0 + Duration::from_millis(50));
        t.check_missed().expect("answered ping should not miss");
        assert_eq!(t.missed(), 0);
    }

    #[test]
    fn tracker_unanswered_ping_increments_miss() {
        let mut t = KeepaliveTracker::new(3);
        let t0 = Instant::now();
        t.record_ping(t0);
        // No pong before the next check.
        t.check_missed().expect("one miss is below limit");
        assert_eq!(t.missed(), 1);
    }

    #[test]
    fn tracker_reaches_limit_returns_keepalive_lost() {
        let mut t = KeepaliveTracker::new(3);
        let t0 = Instant::now();
        for i in 0..3 {
            t.record_ping(t0 + Duration::from_millis(i * 10));
            if i < 2 {
                t.check_missed().expect("not yet at limit");
            } else {
                let err = t.check_missed().expect_err("should hit limit");
                assert!(matches!(err, NetworkError::KeepaliveLost(3)));
            }
        }
    }

    #[test]
    fn tracker_pong_clears_miss_counter() {
        // Ping 1 is unanswered -> miss count 1.  A pong clears the
        // counter.  Subsequent ping+pong pair stays at zero.
        let mut t = KeepaliveTracker::new(3);
        let t0 = Instant::now();
        t.record_ping(t0);
        t.check_missed().unwrap();
        assert_eq!(t.missed(), 1);
        t.record_pong(t0 + Duration::from_secs(1));
        assert_eq!(t.missed(), 0);
        let t1 = t0 + Duration::from_secs(2);
        t.record_ping(t1);
        t.record_pong(t1 + Duration::from_millis(50));
        t.check_missed().unwrap();
        assert_eq!(t.missed(), 0, "answered ping keeps counter at zero");
    }

    // --- ReconnectBackoff ---

    #[test]
    fn backoff_starts_at_initial() {
        let b = ReconnectBackoff::new();
        assert_eq!(b.current(), BACKOFF_INITIAL);
    }

    #[test]
    fn backoff_doubles_until_cap() {
        // Walk the schedule from 1s until we see two consecutive
        // BACKOFF_MAX values, which proves both the doubling and the
        // .min(BACKOFF_MAX) clamp.
        let mut b = ReconnectBackoff::new();
        let mut prev = b.current();
        let mut hit_cap = false;
        for _ in 0..16 {
            b.double();
            let now = b.current();
            if hit_cap {
                assert_eq!(now, BACKOFF_MAX, "must stay at cap once reached");
            } else if now == BACKOFF_MAX {
                hit_cap = true;
            } else {
                assert_eq!(now, prev * 2, "must double exactly until cap");
            }
            prev = now;
        }
        assert!(hit_cap, "backoff must reach BACKOFF_MAX within 16 steps");
    }

    #[test]
    fn backoff_reset_returns_to_initial() {
        let mut b = ReconnectBackoff::new();
        b.double();
        b.double();
        b.double();
        assert_ne!(b.current(), BACKOFF_INITIAL, "precondition: not at start");
        b.reset();
        assert_eq!(b.current(), BACKOFF_INITIAL);
    }

    #[test]
    fn backoff_double_after_cap_stays_at_cap() {
        let mut b = ReconnectBackoff::new();
        // Walk to the cap.
        while b.current() < BACKOFF_MAX {
            b.double();
        }
        assert_eq!(b.current(), BACKOFF_MAX);
        // Further doubles must not overflow or exceed the cap.
        for _ in 0..8 {
            b.double();
            assert_eq!(b.current(), BACKOFF_MAX);
        }
    }

    // --- keepalive_loop ---
    //
    // 1h interval keeps the ticker quiet so the test doesn't race a
    // ping send before recv classifies the master's reply.

    const KEEPALIVE_TEST_CONFIG: &str = r#"
        [repeater]
        callsign = "N0CALL"
        dmr_id = 1234567
        src_id = 1234567
        rx_freq = 434000000
        tx_freq = 439000000

        [usrp]
        local_host = "127.0.0.1"
        local_port = 34001
        remote_host = "127.0.0.1"
        remote_port = 34002

        [vocoder]
        backend = "mbelib"

        [dmr]
        slot = 1
        talkgroup = 9
        call_type = "group"
        hang_time = "500ms"
        stream_timeout = "5s"

        [network]
        profile = "brandmeister"
        host = "test.local"
        port = 62031
        password = "pw"
        keepalive_interval = "1h"
        keepalive_missed_limit = 3
    "#;

    async fn run_keepalive_with_response(response: &[u8]) -> Result<(), NetworkError> {
        use crate::config::Config;
        let parsed: Config = toml::from_str(KEEPALIVE_TEST_CONFIG).unwrap();
        let config = parsed.resolve(SecretString::from("test"), None);
        let master = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge_addr = bridge.local_addr().unwrap();
        bridge.connect(master.local_addr().unwrap()).await.unwrap();

        master.send_to(response, bridge_addr).await.unwrap();

        let (dmrd_tx, _dmrd_rx) = mpsc::channel(8);
        // Bind, don't drop: a dropped sender closes the channel and the
        // loop exits via the `None` arm before reading from the socket.
        let (_voice_out_tx, mut voice_out_rx) = mpsc::channel(8);
        let (_ctl_out_tx, mut ctl_out_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        keepalive_loop(
            &bridge,
            &config,
            &dmrd_tx,
            &mut voice_out_rx,
            &mut ctl_out_rx,
            cancel,
        )
        .await
    }

    #[tokio::test]
    async fn keepalive_loop_returns_master_nak_on_mstnak() {
        let result = run_keepalive_with_response(b"MSTNAK\x00\x12\xD6\x87").await;
        assert!(
            matches!(result, Err(NetworkError::MasterNak { stage: "keepalive" })),
            "expected MasterNak{{stage:keepalive}}, got {result:?}"
        );
    }

    #[tokio::test]
    async fn keepalive_loop_returns_disconnect_on_mstcl() {
        let result = run_keepalive_with_response(b"MSTCL\x00\x12\xD6\x87").await;
        assert!(
            matches!(result, Err(NetworkError::MasterDisconnect)),
            "expected MasterDisconnect, got {result:?}"
        );
    }
}
