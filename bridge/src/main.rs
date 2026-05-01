mod agc;
mod bm_provision;
#[expect(dead_code, reason = "call_type, hang_time consumed in Milestone 5")]
mod config;
mod homebrew_client;
mod network;
#[expect(
    dead_code,
    reason = "to_be_bytes_3, index, as_bytes consumed in Milestone 5"
)]
mod types;
mod usrp;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

/// Upper bound on how long we wait for blocking vocoder tasks to
/// drain during tokio runtime shutdown.  A stuck serial read can
/// hold a `spawn_blocking` thread up to `SERIAL_TIMEOUT` (2s), plus
/// small slack.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

use crate::agc::Agc;
use crate::agc::AgcParams;
use crate::config::Config;
use crate::config::Network;
use crate::config::VocoderBackend;
use crate::network::brandmeister::Brandmeister;
use dmr_events::CallsignLookup;
use dmr_subscriber::Subscribers;

#[derive(Parser)]
#[command(about = "ASL3 to DMR bridge")]
struct Args {
    /// Path to config TOML file
    config: PathBuf,

    /// Read the BM hotspot password from this file (single line,
    /// trailing whitespace stripped).  Alternatives in priority
    /// order: this flag > `BM_BRIDGE_PASSWORD` env var >
    /// `[network].password` in the config.  Exactly one source
    /// must supply the password.
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
}

const PASSWORD_ENV: &str = "BM_BRIDGE_PASSWORD";
const API_KEY_ENV: &str = "BRANDMEISTER_API_KEY";

fn make_profile(profile: &Network) -> Box<dyn network::NetworkProfile> {
    match profile {
        Network::Brandmeister => Box::new(Brandmeister),
    }
}

fn config_gain(config: &config::VocoderConfig) -> Option<(i8, i8)> {
    match (config.gain_in_db, config.gain_out_db) {
        (None, None) => None,
        (a, b) => Some((a.unwrap_or(0), b.unwrap_or(0))),
    }
}

async fn make_vocoder(config: &config::VocoderConfig) -> anyhow::Result<Box<dyn ambe::Vocoder>> {
    match config.backend {
        #[cfg(feature = "thumbdv")]
        VocoderBackend::Thumbdv => {
            let path = config
                .serial_port
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("thumbdv requires serial_port"))?;
            let baud = config.serial_baud;
            let gain = config_gain(config);
            Ok(ambe::open_thumbdv(path, baud, gain)?)
        }
        #[cfg(not(feature = "thumbdv"))]
        VocoderBackend::Thumbdv => {
            anyhow::bail!("thumbdv backend not compiled (enable the 'thumbdv' feature)")
        }
        VocoderBackend::Ambeserver => {
            let host = config
                .host
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("ambeserver requires host"))?;
            let port = config.port.unwrap_or(2460);
            let addr = resolve_socket_addr(host, port).await?;
            let gain = config_gain(config);
            Ok(ambe::open_ambeserver(addr, gain)?)
        }
        #[cfg(feature = "mbelib")]
        VocoderBackend::Mbelib => Ok(ambe::open_mbelib()),
        #[cfg(not(feature = "mbelib"))]
        VocoderBackend::Mbelib => {
            anyhow::bail!("mbelib backend not compiled (enable the 'mbelib' feature)")
        }
    }
}

/// Resolve a `host:port` string (literal IP or hostname) to the
/// first `SocketAddr` returned by the OS resolver.  Mirrors the
/// hostname-friendly path the homebrew client uses for the BM
/// master so operators can put either form in `[usrp]`.
async fn resolve_socket_addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let target = format!("{host}:{port}");
    tokio::net::lookup_host(&target)
        .await
        .with_context(|| format!("resolving {target}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses resolved for {target}"))
}

/// Periodically re-load the subscriber CSV and swap the shared
/// `Arc<Subscribers>` atomically.  A failed reload is logged and the
/// prior table stays in place, so a transient bad CSV doesn't blank
/// out callsign enrichment.
///
/// Runs as a try_join! branch on the main task so panics propagate
/// up cleanly (process exits, systemd restarts).  `Subscribers::load`
/// is sync I/O via the csv crate -- wrapped in `spawn_blocking` so a
/// slow reload doesn't stall the voice / homebrew branches sharing
/// this task.
async fn subscriber_refresh(
    state: Arc<std::sync::RwLock<Arc<Subscribers>>>,
    path: PathBuf,
    interval: Duration,
    cancel: CancellationToken,
) {
    info!(path = %path.display(), interval = ?interval, "subscriber refresh enabled");
    let mut ticker = tokio::time::interval(interval);
    // Avoid back-to-back catch-up reloads if the runtime stalls.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let path_for_load = path.clone();
                let load = tokio::task::spawn_blocking(
                    move || Subscribers::load(&path_for_load),
                ).await;
                match load {
                    Ok(Ok(new_subs)) => {
                        // Recover from poisoning: only op under the lock
                        // is an Arc swap, no torn state to protect.
                        let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                        *guard = Arc::new(new_subs);
                    }
                    Ok(Err(e)) => {
                        warn!(path = %path.display(), "subscriber reload failed: {e}");
                    }
                    Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                    Err(_) => unreachable!("spawn_blocking task is never aborted"),
                }
            }
        }
    }
}

/// Cancels its token on drop, so a panic mid-handler still trips
/// cancel instead of stranding the bridge.  Cancel is idempotent.
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Spawn a task that cancels the token on SIGINT or SIGTERM.
/// SIGTERM registration can fail in restricted sandboxes (seccomp,
/// no rt_sigaction); fall through to SIGINT-only.
fn spawn_signal_handler(cancel: CancellationToken) {
    tokio::spawn(async move {
        let _guard = CancelOnDrop(cancel);
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => info!("SIGINT received"),
                    _ = sigterm.recv() => info!("SIGTERM received"),
                }
            }
            Err(e) => {
                warn!("SIGTERM handler unavailable ({e}); listening for SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                info!("SIGINT received");
            }
        }
    });
}

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async_main());
    runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
    result
}

async fn async_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let mut config = Config::load(&args.config).await?;

    // Resolve BM password from --password-file / env / config in
    // that priority, before logging the config (so we know which
    // source supplied it; the value itself is SecretString-redacted).
    let file_source = match &args.password_file {
        Some(path) => config::read_password_file(path)?,
        None => None,
    };
    let env_source = std::env::var(PASSWORD_ENV)
        .ok()
        .map(secrecy::SecretString::from);
    let password = config::resolve_password(&mut config, file_source, env_source)?;

    let api_key_env = std::env::var(API_KEY_ENV)
        .ok()
        .map(secrecy::SecretString::from);
    config::resolve_api_key(&mut config, api_key_env)?;

    info!(
        callsign = %config.repeater.callsign.as_str(),
        dmr_id = config.repeater.dmr_id.as_u32(),
        tg = config.dmr.talkgroup.as_u32(),
        slot = ?config.dmr.slot,
        gateway = ?config.dmr.gateway,
        "config loaded"
    );
    tracing::debug!("config: {config:#?}");

    // Cancel token + signal handler installed before any blocking
    // startup work, so SIGINT during a slow `provision` (e.g., BM
    // API unreachable) bails cleanly.
    let cancel = CancellationToken::new();
    spawn_signal_handler(cancel.clone());

    // Brandmeister API: anonymous peer-profile log + optional static-TG
    // reconciliation.  Runs before BM master connect so any TG mutations
    // are visible to the master once we authenticate.  All failures are
    // non-fatal; the bridge still services voice traffic without it.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(()),
        () = bm_provision::provision(&config) => {}
    }

    // Optional AGC on the USRP-tx (digital -> analog) path.
    let agc_state: Option<Agc> = if config.agc.enabled {
        Some(Agc::new(AgcParams {
            target_dbfs: config.agc.target_dbfs,
            attack: config.agc.attack,
            release: config.agc.release,
            max_gain_db: config.agc.max_gain_db,
        }))
    } else {
        None
    };

    // Optional callsign-lookup wired into voice_task: enriches USRP
    // TEXT call metadata with `call`/`name` fields when an inbound
    // talker's DMR ID resolves in the configured RadioID CSV.  The
    // closure reads through a shared `RwLock<Arc<Subscribers>>` so a
    // background reload task (spawned below, after `cancel` exists)
    // can swap the table atomically without disturbing live calls.
    let subscribers_state: Option<Arc<std::sync::RwLock<Arc<Subscribers>>>> = config
        .repeater
        .subscriber_file
        .as_deref()
        .map(|path| {
            // Initial load is tolerant: a missing or malformed file
            // logs a warn and starts with an empty table.  The
            // periodic refresh task (when configured) will pick the
            // file up on a later tick.
            let initial = match Subscribers::load(path) {
                Ok(subs) => {
                    info!(path = %path.display(), entries = subs.len(), "loaded DMR subscribers");
                    subs
                }
                Err(e) => {
                    warn!(path = %path.display(), "subscriber file load failed: {e}; starting with empty table");
                    Subscribers::default()
                }
            };
            Arc::new(std::sync::RwLock::new(Arc::new(initial)))
        });
    let callsign_lookup: Option<CallsignLookup> = subscribers_state.as_ref().map(|state| {
        let state = state.clone();
        Arc::new(move |id| {
            // Best-effort lookup: recover through poisoning.
            let snapshot = state.read().unwrap_or_else(|e| e.into_inner()).clone();
            snapshot
                .get(id)
                .map(|s| (s.callsign.clone(), s.first_name.clone()))
        }) as CallsignLookup
    });

    let local_addr = resolve_socket_addr(&config.usrp.local_host, config.usrp.local_port).await?;
    let remote_addr =
        resolve_socket_addr(&config.usrp.remote_host, config.usrp.remote_port).await?;

    let socket = Arc::new(UdpSocket::bind(local_addr).await?);
    info!("USRP listening on {local_addr}, sending to {remote_addr}");

    let byte_swap = config.usrp.byte_swap;

    // Voice-input channels (FM-side audio frames and BM DMRD packets
    // both carry real-time audio feeding voice_task).  Kept shallow
    // so a stalled consumer drops packets quickly rather than
    // buffering seconds of stale audio: 8 audio frames = 160 ms,
    // 8 DMRD packets = 480 ms.  If we can't process that much within
    // that window, dropping is the right failure mode.
    const VOICE_IN_DEPTH: usize = 8;

    // Paced USRP output channel.  voice_task emits 3 frames per
    // incoming DMRD burst (nearly simultaneously); usrp::tx_task
    // drains them at 20 ms pacing.  Steady-state depth hovers near
    // 3; 16 slots = 320 ms of headroom for brief producer bursts.
    const AUDIO_OUT_DEPTH: usize = 16;

    // DMRD output to the network.  Control packets (header,
    // terminator, RPTPING, RPTCL) go through send().await and must
    // not be dropped; keep a wider buffer so a brief homebrew_client
    // stall (e.g. reconnect) doesn't starve them.
    const DMRD_OUT_DEPTH: usize = 64;

    // FM-side rx -> voice task (USRP transport strips wire fields).
    let (audio_in_tx, audio_in_rx) = mpsc::channel(VOICE_IN_DEPTH);

    // DMRD inbound: homebrew_client -> voice task.
    let (dmrd_in_tx, dmrd_in_rx) = mpsc::channel(VOICE_IN_DEPTH);

    // DMRD outbound voice bursts: bounded, best-effort under backpressure.
    let (dmrd_voice_out_tx, dmrd_voice_out_rx) = mpsc::channel(DMRD_OUT_DEPTH);

    // DMRD outbound control packets: headers/terminators must not be
    // dropped just because the bounded voice queue filled first.
    let (dmrd_ctl_out_tx, dmrd_ctl_out_rx) = mpsc::unbounded_channel();

    // Voice PCM: voice task -> USRP tx (paced drain).
    let (audio_out_tx, audio_out_rx) = mpsc::channel(AUDIO_OUT_DEPTH);

    // Transport lifecycle control into voice_task.  Tiny depth is
    // enough: only session-reset notifications flow here, and they
    // are ordered ahead of audio handling by the voice-task select.
    let (voice_ctl_tx, voice_ctl_rx) = mpsc::channel(4);

    // Out-of-band call metadata: voice task -> USRP tx.  Shallow:
    // metadata is best-effort and dropping is preferable to stalling.
    let (metadata_tx, metadata_rx) = mpsc::channel::<dmr_events::MetaEvent>(8);

    let profile = make_profile(&config.network.profile);
    let vocoder = make_vocoder(&config.vocoder).await?;

    let voice_cfg = dmr_wire::voice::VoiceConfig {
        gateway: match config.dmr.gateway {
            config::GatewayMode::Both => dmr_wire::voice::Direction::Both,
            config::GatewayMode::DmrToFm => dmr_wire::voice::Direction::DmrToFm,
            config::GatewayMode::FmToDmr => dmr_wire::voice::Direction::FmToDmr,
        },
        slot: config.dmr.slot,
        talkgroup: config.dmr.talkgroup,
        call_type: match config.dmr.call_type {
            config::CallType::Group => dmr_wire::dmrd::CallType::Group,
            config::CallType::Private => dmr_wire::dmrd::CallType::Unit,
        },
        hang_time: config.dmr.hang_time,
        stream_timeout: config.dmr.stream_timeout,
        tx_timeout: config.dmr.tx_timeout,
        min_tx_hang: config.dmr.min_tx_hang,
        repeater_id: config.repeater.dmr_id,
        src_id: config.repeater.src_id,
        color_code: config.repeater.color_code,
        callsign: config.repeater.callsign.as_str().to_string(),
    };

    // Each branch trips `cancel` on its own exit (Ok, Err, or natural
    // completion).  With `join!` (not `try_join!`) all siblings then
    // drain through their cancel-aware paths -- voice_task gets to
    // run on_shutdown (terminator + unkey to peers) instead of being
    // dropped at its next .await mid-call.
    let voice = async {
        dmr_wire::voice::voice_task(
            dmrd_in_rx,
            audio_in_rx,
            voice_ctl_rx,
            audio_out_tx,
            dmrd_voice_out_tx,
            dmrd_ctl_out_tx,
            metadata_tx,
            callsign_lookup,
            vocoder,
            voice_cfg,
            cancel.clone(),
        )
        .await;
        cancel.cancel();
        anyhow::Ok(())
    };
    let homebrew = async {
        let r = homebrew_client::run(
            &config,
            &password,
            profile.as_ref(),
            dmrd_in_tx,
            dmrd_voice_out_rx,
            dmrd_ctl_out_rx,
            voice_ctl_tx,
            cancel.clone(),
        )
        .await
        .map_err(anyhow::Error::from);
        cancel.cancel();
        r
    };
    let subscriber_branch = async {
        if let (Some(state), Some(path)) = (
            subscribers_state.as_ref(),
            config.repeater.subscriber_file.as_deref(),
        ) && !config.repeater.subscriber_refresh_interval.is_zero()
        {
            subscriber_refresh(
                state.clone(),
                path.to_path_buf(),
                config.repeater.subscriber_refresh_interval,
                cancel.clone(),
            )
            .await;
        }
        cancel.cancel();
        anyhow::Ok(())
    };
    let bm_reconcile_branch = async {
        if let Some(api_cfg) = config.brandmeister_api.as_ref()
            && !api_cfg.reconcile_interval.is_zero()
        {
            bm_provision::periodic_provision(
                config.repeater.dmr_id.as_u32(),
                api_cfg.clone(),
                api_cfg.reconcile_interval,
                cancel.clone(),
            )
            .await;
        }
        cancel.cancel();
        anyhow::Ok(())
    };
    // rx + tx must `async move` to take ownership of `socket` (or a
    // clone) and the channel halves; cancel is cloned per branch so
    // the outer `cancel` stays available for the non-move branches.
    let socket_for_rx = socket.clone();
    let cancel_for_rx = cancel.clone();
    let rx = async move {
        let r = usrp::rx_task(
            socket_for_rx,
            audio_in_tx,
            remote_addr,
            byte_swap,
            cancel_for_rx.clone(),
        )
        .await;
        cancel_for_rx.cancel();
        r
    };
    let tg = config.dmr.talkgroup.as_u32();
    let cancel_for_tx = cancel.clone();
    let tx = async move {
        let r = usrp::tx_task(
            socket,
            audio_out_rx,
            metadata_rx,
            remote_addr,
            tg,
            byte_swap,
            agc_state,
            cancel_for_tx.clone(),
        )
        .await;
        cancel_for_tx.cancel();
        r
    };
    let (r_rx, r_tx, r_hb, r_voice, r_sub, r_bm) = tokio::join!(
        rx,
        tx,
        homebrew,
        voice,
        subscriber_branch,
        bm_reconcile_branch
    );
    if let Some(e) = [r_rx, r_tx, r_hb, r_voice, r_sub, r_bm]
        .into_iter()
        .find_map(Result::err)
    {
        return Err(e);
    }

    info!("shutdown complete");
    Ok(())
}
