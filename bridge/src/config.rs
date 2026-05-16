use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::ExposeSecret;
use secrecy::SecretString;
use serde::Deserialize;

use dmr_types::ColorCode;
use dmr_types::DmrId;
use dmr_types::Slot;
use dmr_types::SubscriberId;
use dmr_types::Talkgroup;

use crate::types::Callsign;
use crate::types::Frequency;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error(
        "no BM password supplied (set [network].password in config, \
         BRANDMEISTER_PASSWORD env var, [network].password_file, or --password-file)"
    )]
    PasswordMissing,

    #[error("BM password set in multiple sources: {0:?} (pick one)")]
    PasswordAmbiguous(Vec<&'static str>),

    #[error("reading password file {path}")]
    PasswordFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Password file contains an embedded newline that isn't just
    /// the trailing line terminator -- ambiguous which line is the
    /// actual password.  Reject rather than silently using
    /// `"line1\nline2"` as the secret.
    #[error("password file {path} has multiple lines; expected a single-line password")]
    PasswordFileMultiline { path: PathBuf },

    /// `network.keepalive_interval = "0s"` would panic
    /// `tokio::time::interval` at startup; reject at load time.
    #[error("network.keepalive_interval must be > 0")]
    KeepaliveIntervalZero,

    #[error("brandmeister_api.api_key and brandmeister_api.api_key_file both set (pick one)")]
    BrandmeisterApiKeyAmbiguous,

    #[error(
        "brandmeister_api static_talkgroups_ts{slot} declared but no API key supplied \
         (set api_key, api_key_file, or BRANDMEISTER_API_KEY env var)"
    )]
    BrandmeisterApiKeyMissingForStatics { slot: u8 },

    #[error("reading brandmeister_api.api_key_file {path}")]
    BrandmeisterApiKeyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("brandmeister_api.api_key_file {path} has multiple lines; expected single-line JWT")]
    BrandmeisterApiKeyFileMultiline { path: PathBuf },

    #[cfg(feature = "neural")]
    #[error(
        "[vocoder.neural] encoder_backend and decoder_backend cannot both be non-neural; \
         use a dedicated backend instead"
    )]
    NeuralBothNonNeural,
}

/// Top-level configuration, mirrors DESIGN.md configuration schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) peer: PeerConfig,
    pub(crate) usrp: UsrpConfig,
    pub(crate) vocoder: VocoderConfig,
    pub(crate) dmr: DmrConfig,
    pub(crate) network: NetworkConfig,
    /// Optional: enables Brandmeister API integration (anonymous peer
    /// profile log on startup, plus pure-set static-TG reconciliation
    /// when api_key + static lists are supplied).  Section absent =
    /// no API calls at all.
    #[serde(default)]
    pub(crate) brandmeister_api: Option<BrandmeisterApiConfig>,
    /// Optional per-direction automatic gain control.  Both
    /// directions off by default.
    #[serde(default)]
    pub(crate) agc: AgcSection,
    /// Per-call summary log + periodic heartbeat counters.  Section
    /// absent uses the defaults: 60s heartbeat, idle-suppress on,
    /// 250ms minimum-call threshold for per-call lines.  See
    /// `bridge/src/stats.rs`.
    #[serde(default)]
    pub(crate) stats: StatsConfig,
    /// Diagnostic capture knobs (per-call WAV recording, etc.).
    /// Section absent = all diagnostics off.
    #[serde(default)]
    pub(crate) diagnostics: DiagnosticsConfig,
    /// FM->DMR pre-encode voice-band filter (HP4 @ 250 Hz + LP2 @
    /// 3000 Hz).  Default on; set `enabled = false` to bypass.
    #[serde(default)]
    pub(crate) encode_filter: EncodeFilterConfig,
    /// Static dB gain applied in each direction.  Backend-agnostic
    /// (works with neural and dynarmic, which ignore the DV3000
    /// chip-side `vocoder.gain_*_db`).
    #[serde(default)]
    pub(crate) gain: GainConfig,
    /// Per-direction brick-wall peak limiter applied after AGC.
    /// Both directions off by default.
    #[serde(default)]
    pub(crate) limiter: LimiterSection,
}

/// Static dB gain knobs, applied per direction.  Defaults 0 dB
/// (unity).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GainConfig {
    /// dB applied to USRP-rx PCM just before the vocoder encode
    /// step.
    pub(crate) fm_to_dmr_db: f32,
    /// dB applied to vocoder-decoded PCM just after decode (before
    /// AGC and the USRP-tx send).
    pub(crate) dmr_to_fm_db: f32,
}

/// Per-direction AGC parameters; `enabled = false` skips processing
/// entirely so the path stays bit-exact when AGC is off.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AgcConfig {
    pub(crate) enabled: bool,
    pub(crate) target_dbfs: f32,
    #[serde(with = "humantime_serde")]
    pub(crate) attack: Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) release: Duration,
    pub(crate) max_gain_db: f32,
    pub(crate) noise_gate_dbfs: f32,
}

impl Default for AgcConfig {
    /// Defaults shared by both directions; per-direction tuning lives
    /// in `AgcSection::default`.
    fn default() -> Self {
        Self {
            enabled: false,
            target_dbfs: -6.0,
            attack: Duration::from_millis(10),
            release: Duration::from_millis(120),
            max_gain_db: 18.0,
            noise_gate_dbfs: -50.0,
        }
    }
}

/// Both AGC directions, configured independently.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AgcSection {
    /// AGC on the DMR -> FM (vocoder-decoded PCM out to USRP) path.
    pub(crate) dmr_to_fm: AgcConfig,
    /// AGC on the FM -> DMR (USRP PCM in to vocoder) path.  Default
    /// target is hotter (-3 dBFS) than dmr_to_fm because the FM-side
    /// input has wider variance and tends to come in quiet.
    pub(crate) fm_to_dmr: AgcConfig,
}

impl Default for AgcSection {
    fn default() -> Self {
        Self {
            dmr_to_fm: AgcConfig::default(),
            fm_to_dmr: AgcConfig {
                target_dbfs: -3.0,
                ..AgcConfig::default()
            },
        }
    }
}

/// Per-direction brick-wall peak limiter.  `enabled = false` (default)
/// skips processing entirely.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LimiterConfig {
    pub(crate) enabled: bool,
    /// Hard ceiling in dBFS (negative).  Frames whose peak exceeds this
    /// are scaled down so the peak equals the ceiling exactly.
    pub(crate) ceiling_dbfs: f32,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ceiling_dbfs: -1.0,
        }
    }
}

/// Both limiter directions, configured independently.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LimiterSection {
    /// Limiter on the DMR -> FM (vocoder-decoded PCM out to USRP) path.
    pub(crate) dmr_to_fm: LimiterConfig,
    /// Limiter on the FM -> DMR (USRP PCM in to vocoder) path.
    pub(crate) fm_to_dmr: LimiterConfig,
}

/// Per-call + heartbeat stats logging.  All fields optional; section
/// absent leaves the bridge with the defaults.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StatsConfig {
    /// Period for the cumulative-counters heartbeat log.  `0s` disables
    /// the heartbeat entirely (per-call lines still emit).
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    /// When true, a heartbeat tick that saw zero new frames in either
    /// direction since the previous tick is suppressed.  Predictable
    /// cadence ops can prefer false; quiet logs prefer true.
    pub(crate) skip_idle_heartbeat: bool,
    /// Per-call summary lines below this duration are suppressed.
    /// Cumulative counters still see the call's frames + drops.
    /// Filters out PTT-tap noise.
    #[serde(with = "humantime_serde")]
    pub(crate) min_call_log_duration: Duration,
    /// Period for the delta-counters rollup log.  Logs calls, frames,
    /// and drops per direction since the previous rollup.  `0s` disables.
    #[serde(with = "humantime_serde")]
    pub(crate) summary_interval: Duration,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(60),
            skip_idle_heartbeat: true,
            min_call_log_duration: Duration::from_millis(250),
            summary_interval: Duration::from_secs(12 * 3600),
        }
    }
}

/// Optional Brandmeister Halligan API integration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrandmeisterApiConfig {
    /// Bearer JWT.  Resolution: this field, `api_key_file`, or
    /// `BRANDMEISTER_API_KEY` env var -- exactly one.  After
    /// `Config::load`, this is `Some(resolved)` if any source
    /// supplied a key, else `None`.
    #[serde(default)]
    pub(crate) api_key: Option<SecretString>,
    /// Path to a single-line file containing the bearer JWT.
    /// Mutually exclusive with `api_key`; the env var is checked
    /// separately at startup.
    #[serde(default)]
    pub(crate) api_key_file: Option<PathBuf>,
    /// Desired TS1 static talkgroup list.  Pure-set reconciliation:
    /// missing TGs are POSTed, extras are DELETEd.  Empty list =
    /// remove all TS1 statics.  Omit (None) = leave TS1 untouched.
    #[serde(default)]
    pub(crate) static_talkgroups_ts1: Option<Vec<dmr_types::Talkgroup>>,
    /// Same semantics as `static_talkgroups_ts1`, for TS2.
    #[serde(default)]
    pub(crate) static_talkgroups_ts2: Option<Vec<dmr_types::Talkgroup>>,
    /// Optional periodic re-run of the startup peer-profile log +
    /// static-TG reconciliation.  Default `0` runs once at startup
    /// only.  A positive duration spawns a background task that
    /// repeats `provision` on each tick, so SelfCare edits made
    /// while the bridge is up get corrected on the next pass.
    #[serde(with = "humantime_serde", default = "default_bm_reconcile_interval")]
    pub(crate) reconcile_interval: Duration,
}

fn default_bm_reconcile_interval() -> Duration {
    Duration::ZERO
}

/// Repeater identity and metadata sent in the RPTC config packet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerConfig {
    pub(crate) callsign: Callsign,
    pub(crate) dmr_id: DmrId,
    /// On-air DMR subscriber ID (24-bit), used as the `src_id` in
    /// the DMRD wire body and embedded LC.  Required because
    /// `dmr_id` (32-bit Homebrew repeater identity) can exceed 24
    /// bits for BM hotspots and must not be reused as src_id --
    /// truncation would alias onto an unrelated subscriber.
    pub(crate) src_id: SubscriberId,
    pub(crate) rx_freq: Frequency,
    pub(crate) tx_freq: Frequency,
    #[serde(default)]
    pub(crate) tx_power: String,
    #[serde(default = "default_color_code")]
    pub(crate) color_code: ColorCode,
    /// Optional decimal degrees.  serde rejects malformed input at
    /// load time; missing means "unset" and wires as 0.0.
    #[serde(default)]
    pub(crate) latitude: Option<f64>,
    #[serde(default)]
    pub(crate) longitude: Option<f64>,
    /// Antenna height in meters.  serde rejects malformed input at
    /// load time; missing means "unset" and wires as 0.
    #[serde(default)]
    pub(crate) height: Option<u32>,
    #[serde(default)]
    pub(crate) location: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) url: String,
    /// Optional path to a RadioID-style subscriber CSV
    /// (`RADIO_ID,CALLSIGN,FIRST_NAME,LAST_NAME,CITY,STATE,COUNTRY`).
    /// When set, USRP TEXT call metadata gains `call` + `name`
    /// fields populated from this lookup; absent or unmatched IDs
    /// just omit those fields.
    #[serde(default)]
    pub(crate) subscriber_file: Option<PathBuf>,
    /// Optional periodic re-load of `subscriber_file`.  Default `0`
    /// = load once at startup only (existing behavior).  A positive
    /// duration spawns a background task that re-parses the CSV on
    /// each tick; a parse failure is logged and the prior table
    /// stays in place, so a transient corruption can't blank out
    /// callsign enrichment.
    #[serde(
        with = "humantime_serde",
        default = "default_subscriber_refresh_interval"
    )]
    pub(crate) subscriber_refresh_interval: Duration,
}

fn default_subscriber_refresh_interval() -> Duration {
    Duration::ZERO
}

fn default_color_code() -> ColorCode {
    ColorCode::default()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsrpConfig {
    pub(crate) local_host: String,
    pub(crate) local_port: u16,
    pub(crate) remote_host: String,
    pub(crate) remote_port: u16,
    /// Swap audio sample bytes for cross-endian USRP peers.
    #[serde(default)]
    pub(crate) byte_swap: bool,
}

/// Vocoder backend selection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VocoderBackend {
    #[serde(rename = "thumbdv")]
    ThumbDV,
    Ambeserver,
    Neural,
    Dynarmic,
}

/// ThumbDV serial hardware configuration (`[vocoder.thumbdv]`).
#[cfg(feature = "thumbdv")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThumbDvConfig {
    pub(crate) serial_port: String,
    /// Serial baud rate; defaults to 460800 if absent.
    pub(crate) serial_baud: Option<u32>,
}

/// AMBEserver UDP-proxy configuration (`[vocoder.ambeserver]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AmbeserverConfig {
    pub(crate) host: String,
    /// UDP port; defaults to 2460 if absent.
    pub(crate) port: Option<u16>,
}

/// Encoder/decoder backend for one direction of the neural vocoder
/// (`[vocoder.neural]`).
#[cfg(feature = "neural")]
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NeuralHalf {
    Neural,
    Dynarmic,
    #[serde(rename = "thumbdv")]
    ThumbDV,
    Ambeserver,
}

/// Step kernel for the neural decoder (`[vocoder.neural.decoder]`).
#[cfg(feature = "neural")]
#[derive(Debug, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NeuralDecoderStep {
    /// Native Rust GRU kernel (default); requires `weights_dir`.
    #[default]
    NativeGru,
    /// ONNX step model via tract; requires `split_dir` to contain
    /// `decoder_step.onnx`.
    Onnx,
}

/// Neural decoder sub-configuration (`[vocoder.neural.decoder]`).
/// Required when `decoder_backend = "neural"`.
#[cfg(feature = "neural")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NeuralDecoderConfig {
    /// Step kernel; defaults to `native_gru`.
    #[serde(default)]
    pub(crate) step: NeuralDecoderStep,
    /// Directory containing `decoder_frame.onnx` (always required); also
    /// needs `decoder_step.onnx` when `step = "onnx"`.
    pub(crate) split_dir: std::path::PathBuf,
    /// Directory containing flat-binary GRU weight files (`W_ir.bin`, etc.);
    /// required when `step = "native_gru"` (the default).
    pub(crate) weights_dir: Option<std::path::PathBuf>,
}

#[cfg(feature = "neural")]
fn default_neural_encoder_backend() -> NeuralHalf {
    NeuralHalf::Neural
}

#[cfg(feature = "neural")]
fn default_neural_decoder_backend() -> NeuralHalf {
    NeuralHalf::Dynarmic
}

/// Neural vocoder configuration (`[vocoder.neural]`).  Both directions
/// independently selectable.  At least one must be `neural` (validated
/// at load time).
#[cfg(feature = "neural")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NeuralVocoderConfig {
    /// Encoder backend; defaults to `neural`.
    #[serde(default = "default_neural_encoder_backend")]
    pub(crate) encoder_backend: NeuralHalf,
    /// Decoder backend; defaults to `dynarmic`.
    #[serde(default = "default_neural_decoder_backend")]
    pub(crate) decoder_backend: NeuralHalf,
    /// ONNX model path; required when `encoder_backend = "neural"`.
    pub(crate) encoder_model_path: Option<std::path::PathBuf>,
    /// Decoder sub-config; required when `decoder_backend = "neural"`.
    pub(crate) decoder: Option<NeuralDecoderConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VocoderConfig {
    pub(crate) backend: VocoderBackend,
    #[cfg(feature = "thumbdv")]
    pub(crate) thumbdv: Option<ThumbDvConfig>,
    pub(crate) ambeserver: Option<AmbeserverConfig>,
    #[cfg(feature = "neural")]
    pub(crate) neural: Option<NeuralVocoderConfig>,
}

/// DMR call type: group or private.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CallType {
    Group,
    Private,
}

/// Which directions the bridge forwards voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayMode {
    /// Both directions: FM->DMR and DMR->FM.
    Both,
    /// DMR->FM only (listen only, no transmit to DMR network).
    DmrToFm,
    /// FM->DMR only (transmit only, no decode from DMR network).
    FmToDmr,
}

fn default_gateway_mode() -> GatewayMode {
    GatewayMode::Both
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DmrConfig {
    #[serde(default = "default_gateway_mode")]
    pub(crate) gateway: GatewayMode,
    pub(crate) slot: Slot,
    pub(crate) talkgroup: Talkgroup,
    pub(crate) call_type: CallType,
    #[serde(with = "humantime_serde")]
    pub(crate) hang_time: Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) stream_timeout: Duration,
    #[serde(with = "humantime_serde", default = "default_tx_timeout")]
    pub(crate) tx_timeout: Duration,
}

/// Stuck-key safeguard for FM-side PTT that never releases.
fn default_tx_timeout() -> Duration {
    Duration::from_secs(180)
}

/// DMR network selection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Network {
    Brandmeister,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkConfig {
    pub(crate) profile: Network,
    pub(crate) host: String,
    pub(crate) port: u16,
    /// Inline password (one of four sources; see `resolve_password`).
    /// `SecretString` keeps the value out of `Debug` and zeroizes on
    /// drop.  Moved out at resolution time and left as `None`.
    #[serde(default)]
    pub(crate) password: Option<SecretString>,
    /// Path to a single-line file containing the password.  Mirrors
    /// `[brandmeister_api].api_key_file`.  Default packaged path is
    /// `/etc/asl-dmr-bridge/password`; operators populate the file
    /// (mode 0600) and reference it from the config.
    #[serde(default)]
    pub(crate) password_file: Option<PathBuf>,
    #[serde(with = "humantime_serde")]
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_missed_limit: u32,
}

/// Fully-resolved runtime configuration.  Constructed only via
/// `RuntimeConfig::load` (or the `Config::resolve` test helper),
/// which does parse + validate + secret-resolve in one step.  Every
/// field is its final runtime shape: no `Option<password>` race,
/// no put-back-into-Option round-trip on `api_key`.
#[derive(Debug)]
pub(crate) struct RuntimeConfig {
    pub(crate) peer: PeerConfig,
    pub(crate) usrp: UsrpConfig,
    pub(crate) vocoder: VocoderConfig,
    pub(crate) dmr: DmrConfig,
    pub(crate) network: ResolvedNetworkConfig,
    pub(crate) brandmeister_api: Option<ResolvedBrandmeisterApiConfig>,
    pub(crate) agc: AgcSection,
    pub(crate) stats: StatsConfig,
    pub(crate) diagnostics: DiagnosticsConfig,
    pub(crate) encode_filter: EncodeFilterConfig,
    pub(crate) gain: GainConfig,
    pub(crate) limiter: LimiterSection,
}

/// Diagnostic capture knobs.  All optional, off by default.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DiagnosticsConfig {
    /// Per-call PCM capture directory.  When set, the bridge writes
    /// 8 kHz mono int16 LE WAV files (one per call per direction).
    pub(crate) pcm_record_dir: Option<std::path::PathBuf>,
}

/// Pre-encode filter on the FM->DMR path; off by default.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EncodeFilterConfig {
    pub(crate) enabled: bool,
}

/// Network section after password resolution.  Mirrors `NetworkConfig`
/// but `password` is the resolved `SecretString`, not an `Option`.
#[derive(Debug)]
pub(crate) struct ResolvedNetworkConfig {
    pub(crate) profile: Network,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) password: SecretString,
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_missed_limit: u32,
}

/// Brandmeister API section after key resolution.  `api_key` stays
/// `Option` because it really is optional (anonymous reads work);
/// `None` here means "no source supplied a key", not "race".
#[derive(Debug, Clone)]
pub(crate) struct ResolvedBrandmeisterApiConfig {
    pub(crate) api_key: Option<SecretString>,
    pub(crate) static_talkgroups_ts1: Option<Vec<dmr_types::Talkgroup>>,
    pub(crate) static_talkgroups_ts2: Option<Vec<dmr_types::Talkgroup>>,
    pub(crate) reconcile_interval: Duration,
}

impl Config {
    pub(crate) async fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        Self::parse_str(path, &text)
    }

    /// Parse + validate config text.  Factored out of `load` so tests
    /// can exercise the TOML surface without hitting the filesystem.
    fn parse_str(path: &Path, text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.network.keepalive_interval.is_zero() {
            return Err(ConfigError::KeepaliveIntervalZero);
        }
        #[cfg(feature = "neural")]
        if let VocoderBackend::Neural = self.vocoder.backend
            && let Some(nc) = &self.vocoder.neural
            && !matches!(nc.encoder_backend, NeuralHalf::Neural)
            && !matches!(nc.decoder_backend, NeuralHalf::Neural)
        {
            return Err(ConfigError::NeuralBothNonNeural);
        }
        Ok(())
    }

    /// Stitch a parsed/validated `Config` together with externally-
    /// resolved password and API key into the runtime shape.  Used
    /// by `RuntimeConfig::load` and by tests that want to hand-build
    /// a runtime config without touching the filesystem.
    pub(crate) fn resolve(
        self,
        password: SecretString,
        api_key: Option<SecretString>,
    ) -> RuntimeConfig {
        let Config {
            peer,
            usrp,
            vocoder,
            dmr,
            network,
            brandmeister_api,
            agc,
            stats,
            diagnostics,
            encode_filter,
            gain,
            limiter,
        } = self;
        RuntimeConfig {
            peer,
            usrp,
            vocoder,
            dmr,
            network: ResolvedNetworkConfig {
                profile: network.profile,
                host: network.host,
                port: network.port,
                password,
                keepalive_interval: network.keepalive_interval,
                keepalive_missed_limit: network.keepalive_missed_limit,
            },
            brandmeister_api: brandmeister_api.map(|api| ResolvedBrandmeisterApiConfig {
                api_key,
                static_talkgroups_ts1: api.static_talkgroups_ts1,
                static_talkgroups_ts2: api.static_talkgroups_ts2,
                reconcile_interval: api.reconcile_interval,
            }),
            agc,
            stats,
            diagnostics,
            encode_filter,
            gain,
            limiter,
        }
    }
}

impl RuntimeConfig {
    /// One-shot constructor: load + parse + validate + resolve secrets.
    /// Returns a fully-resolved config; the caller never sees a
    /// partially-resolved `Config`.
    pub(crate) async fn load(
        path: &Path,
        password_cli_file: Option<SecretString>,
        password_env: Option<SecretString>,
        api_key_cli_file: Option<SecretString>,
        api_key_env: Option<SecretString>,
    ) -> Result<Self, ConfigError> {
        let mut config = Config::load(path).await?;
        let password = resolve_password(&mut config, password_cli_file, password_env)?;
        let api_key = resolve_api_key(&mut config, api_key_cli_file, api_key_env)?;
        Ok(config.resolve(password, api_key))
    }
}

/// Single-line secret-file parser.  Strips edge whitespace (incl. CR
/// for CRLF files and the trailing LF), but rejects embedded newlines:
/// a file like "line1\nline2\n" is ambiguous and silently using the
/// concatenated `"line1\nline2"` as a secret would be a foot-gun.
/// Returns `Ok(None)` for an empty / whitespace-only file so the
/// caller treats it as "not supplied".
enum SecretFileError {
    Io(std::io::Error),
    Multiline,
}

fn read_secret_file(path: &Path) -> Result<Option<SecretString>, SecretFileError> {
    let raw = std::fs::read_to_string(path).map_err(SecretFileError::Io)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains('\n') {
        return Err(SecretFileError::Multiline);
    }
    Ok(Some(SecretString::from(trimmed.to_owned())))
}

/// Read a password file.  See `read_secret_file` for parsing rules.
pub(crate) fn read_password_file(path: &Path) -> Result<Option<SecretString>, ConfigError> {
    read_secret_file(path).map_err(|e| match e {
        SecretFileError::Io(source) => ConfigError::PasswordFile {
            path: path.to_path_buf(),
            source,
        },
        SecretFileError::Multiline => ConfigError::PasswordFileMultiline {
            path: path.to_path_buf(),
        },
    })
}

/// Resolve the BM password from any of four sources:
/// `--password-file` CLI, `BRANDMEISTER_PASSWORD` env var,
/// `[network].password_file` in config, `[network].password` inline.
/// Exactly one must supply a non-empty value; zero is
/// `PasswordMissing`, more than one is `PasswordAmbiguous`.  Returns
/// the resolved secret directly so the caller holds a `SecretString`
/// rather than an `Option<SecretString>` field invariant.
pub(crate) fn resolve_password(
    config: &mut Config,
    cli_file_source: Option<SecretString>,
    env_source: Option<SecretString>,
) -> Result<SecretString, ConfigError> {
    fn non_empty(s: SecretString) -> Option<SecretString> {
        if s.expose_secret().is_empty() {
            None
        } else {
            Some(s)
        }
    }
    let config_file_source = match config.network.password_file.take() {
        Some(path) => read_password_file(&path)?,
        None => None,
    };
    let candidates: Vec<(&'static str, SecretString)> = [
        ("--password-file", cli_file_source),
        ("BRANDMEISTER_PASSWORD", env_source),
        ("config.toml [network].password_file", config_file_source),
        (
            "config.toml [network].password",
            config.network.password.take(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, opt)| opt.and_then(non_empty).map(|s| (name, s)))
    .collect();

    match candidates.len() {
        0 => Err(ConfigError::PasswordMissing),
        1 => {
            let (source, secret) = candidates
                .into_iter()
                .next()
                .expect("candidates.len() == 1 by match arm");
            tracing::info!(source, "loaded BM password");
            Ok(secret)
        }
        _ => Err(ConfigError::PasswordAmbiguous(
            candidates.into_iter().map(|(n, _)| n).collect(),
        )),
    }
}

/// Read a Brandmeister API key file.  See `read_secret_file` for
/// parsing rules.
pub(crate) fn read_api_key_file(path: &Path) -> Result<Option<SecretString>, ConfigError> {
    read_secret_file(path).map_err(|e| match e {
        SecretFileError::Io(source) => ConfigError::BrandmeisterApiKeyFile {
            path: path.to_path_buf(),
            source,
        },
        SecretFileError::Multiline => ConfigError::BrandmeisterApiKeyFileMultiline {
            path: path.to_path_buf(),
        },
    })
}

/// Resolve the Brandmeister API key from up to four sources:
/// `--api-key-file` CLI, `BRANDMEISTER_API_KEY` env var,
/// `[brandmeister_api].api_key_file`, or `[brandmeister_api].api_key`.
/// At most one may apply.  Returns the resolved secret directly.
///
/// Unlike the BM password, the API key is *optional*: anonymous
/// reads (peer profile log) work without it, so missing key just
/// means "no write-path provisioning" rather than a startup error.
/// The caller validates "key required for declared statics".
pub(crate) fn resolve_api_key(
    config: &mut Config,
    cli_file_source: Option<SecretString>,
    env_source: Option<SecretString>,
) -> Result<Option<SecretString>, ConfigError> {
    let Some(api_cfg) = config.brandmeister_api.as_mut() else {
        if cli_file_source.is_some() || env_source.is_some() {
            tracing::warn!(
                "API key supplied via CLI / env but no [brandmeister_api] \
                 section in config; key ignored"
            );
        }
        return Ok(None);
    };
    fn non_empty(s: SecretString) -> Option<SecretString> {
        if s.expose_secret().is_empty() {
            None
        } else {
            Some(s)
        }
    }
    let config_file_source = match api_cfg.api_key_file.take() {
        Some(path) => read_api_key_file(&path)?,
        None => None,
    };
    let candidates: Vec<(&'static str, SecretString)> = [
        ("--api-key-file", cli_file_source),
        ("BRANDMEISTER_API_KEY", env_source),
        ("brandmeister_api.api_key_file", config_file_source),
        ("brandmeister_api.api_key", api_cfg.api_key.take()),
    ]
    .into_iter()
    .filter_map(|(name, opt)| opt.and_then(non_empty).map(|s| (name, s)))
    .collect();

    match candidates.len() {
        0 => {
            // No key supplied -- enforce that no statics are declared
            // (we cannot reconcile without auth).
            if api_cfg.static_talkgroups_ts1.is_some() {
                return Err(ConfigError::BrandmeisterApiKeyMissingForStatics { slot: 1 });
            }
            if api_cfg.static_talkgroups_ts2.is_some() {
                return Err(ConfigError::BrandmeisterApiKeyMissingForStatics { slot: 2 });
            }
            Ok(None)
        }
        1 => {
            let (source, secret) = candidates
                .into_iter()
                .next()
                .expect("candidates.len() == 1 by match arm");
            tracing::info!(source, "loaded Brandmeister API key");
            Ok(Some(secret))
        }
        _ => Err(ConfigError::BrandmeisterApiKeyAmbiguous),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid config text; tests tweak one field at a time.
    const MINIMAL: &str = r#"
[peer]
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
backend = "dynarmic"

[dmr]
slot = 1
talkgroup = 9
call_type = "group"
hang_time = "500ms"
stream_timeout = "5s"

[network]
profile = "brandmeister"
host = "example.invalid"
port = 62031
password = "pw"
keepalive_interval = "5s"
keepalive_missed_limit = 3
"#;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse_str(Path::new("test.toml"), text)
    }

    #[test]
    fn parse_minimal_valid() {
        let cfg = parse(MINIMAL).expect("minimal config parses");
        assert_eq!(cfg.peer.callsign.as_str(), "N0CALL");
        assert_eq!(cfg.peer.dmr_id.as_u32(), 1234567);
        assert_eq!(cfg.dmr.slot, Slot::One);
        assert_eq!(cfg.dmr.talkgroup.as_u32(), 9);
        assert!(matches!(cfg.dmr.gateway, GatewayMode::Both));
    }

    #[test]
    fn malformed_toml_returns_parse_error() {
        let bad = "[peer]\ncallsign = ";
        assert!(matches!(parse(bad), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn missing_required_field_is_parse_error() {
        // Strip the `callsign` line; serde reports the missing field
        // via toml::de::Error, which lands in ConfigError::Parse.
        let text = MINIMAL.replace("callsign = \"N0CALL\"\n", "");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn unknown_field_is_parse_error() {
        // deny_unknown_fields catches typos like `gain_in_dB` that
        // would otherwise silently default and ship wrong behavior.
        let text = MINIMAL.replace("[dmr]", "[dmr]\nbogus_typo = 1");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn invalid_latitude_rejected_at_load() {
        let text = MINIMAL.replace(
            "callsign = \"N0CALL\"",
            "callsign = \"N0CALL\"\nlatitude = \"not-a-number\"",
        );
        assert!(
            matches!(parse(&text), Err(ConfigError::Parse { .. })),
            "expected toml type error for non-numeric latitude"
        );
    }

    #[test]
    fn invalid_height_rejected_at_load() {
        let text = MINIMAL.replace(
            "callsign = \"N0CALL\"",
            "callsign = \"N0CALL\"\nheight = -5",
        );
        assert!(
            matches!(parse(&text), Err(ConfigError::Parse { .. })),
            "expected toml type error for negative height u32"
        );
    }

    #[test]
    fn keepalive_interval_zero_rejected() {
        let text = MINIMAL.replace("keepalive_interval = \"5s\"", "keepalive_interval = \"0s\"");
        let err = parse(&text).expect_err("zero keepalive_interval rejected");
        assert!(
            matches!(err, ConfigError::KeepaliveIntervalZero),
            "got {err:?}"
        );
    }

    #[test]
    fn unknown_vocoder_backend_is_parse_error() {
        let text = MINIMAL.replace("backend = \"dynarmic\"", "backend = \"bogus\"");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn unknown_network_profile_is_parse_error() {
        let text = MINIMAL.replace("profile = \"brandmeister\"", "profile = \"dmr-plus\"");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn debug_redacts_network_password() {
        let cfg = parse(MINIMAL).unwrap();
        let dbg = format!("{:?}", cfg.network);
        assert!(!dbg.contains("pw"), "password leaked into Debug: {dbg}");
        // `secrecy::SecretString` renders as `Secret([REDACTED ...])`
        // in Debug; check for the marker rather than a specific
        // string the upstream library may change.
        assert!(
            dbg.to_uppercase().contains("REDACTED"),
            "expected REDACTED marker, got {dbg}"
        );
    }

    // --- resolve_password ---

    fn parse_no_password(text: &str) -> Config {
        parse(&text.replace("password = \"pw\"\n", "")).unwrap()
    }

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    #[test]
    fn resolve_uses_config_when_only_source() {
        let mut cfg = parse(MINIMAL).unwrap();
        let pw = resolve_password(&mut cfg, None, None).expect("config-only should resolve");
        assert_eq!(pw.expose_secret(), "pw");
        assert!(cfg.network.password.is_none(), "secret was moved out");
    }

    #[test]
    fn resolve_uses_file_when_only_source() {
        let mut cfg = parse_no_password(MINIMAL);
        let pw = resolve_password(&mut cfg, Some(secret("filepw")), None).unwrap();
        assert_eq!(pw.expose_secret(), "filepw");
    }

    #[test]
    fn resolve_uses_env_when_only_source() {
        let mut cfg = parse_no_password(MINIMAL);
        let pw = resolve_password(&mut cfg, None, Some(secret("envpw"))).unwrap();
        assert_eq!(pw.expose_secret(), "envpw");
    }

    #[test]
    fn resolve_errors_if_missing_everywhere() {
        let mut cfg = parse_no_password(MINIMAL);
        let err = resolve_password(&mut cfg, None, None).unwrap_err();
        assert!(matches!(err, ConfigError::PasswordMissing), "got {err:?}");
    }

    #[test]
    fn resolve_errors_if_two_sources_set() {
        // file + env: ambiguous.
        let mut cfg = parse_no_password(MINIMAL);
        let err = resolve_password(&mut cfg, Some(secret("fp")), Some(secret("ep"))).unwrap_err();
        assert!(
            matches!(err, ConfigError::PasswordAmbiguous(ref v) if v.len() == 2),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_errors_if_file_and_config_both_set() {
        let mut cfg = parse(MINIMAL).unwrap(); // config has "pw"
        let err = resolve_password(&mut cfg, Some(secret("fp")), None).unwrap_err();
        assert!(
            matches!(err, ConfigError::PasswordAmbiguous(_)),
            "got {err:?}"
        );
    }

    // --- read_password_file ---

    fn write_temp(contents: &[u8]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents).unwrap();
        f
    }

    #[test]
    fn read_password_file_strips_trailing_newline() {
        let f = write_temp(b"hunter2\n");
        let pw = read_password_file(f.path()).unwrap().unwrap();
        assert_eq!(pw.expose_secret(), "hunter2");
    }

    #[test]
    fn read_password_file_strips_crlf() {
        let f = write_temp(b"hunter2\r\n");
        let pw = read_password_file(f.path()).unwrap().unwrap();
        assert_eq!(pw.expose_secret(), "hunter2");
    }

    #[test]
    fn read_password_file_strips_edge_whitespace() {
        let f = write_temp(b"  hunter2  \n");
        let pw = read_password_file(f.path()).unwrap().unwrap();
        assert_eq!(pw.expose_secret(), "hunter2");
    }

    #[test]
    fn read_password_file_empty_returns_none() {
        let f = write_temp(b"");
        assert!(read_password_file(f.path()).unwrap().is_none());
    }

    #[test]
    fn read_password_file_whitespace_only_returns_none() {
        let f = write_temp(b"   \n\t\n");
        assert!(read_password_file(f.path()).unwrap().is_none());
    }

    #[test]
    fn read_password_file_rejects_embedded_newlines() {
        // The reviewer-flagged foot-gun: a multi-line file silently
        // becoming "line1\nline2" as the password.  Reject instead.
        let f = write_temp(b"hunter2\nextra\n");
        let err = read_password_file(f.path()).unwrap_err();
        assert!(
            matches!(err, ConfigError::PasswordFileMultiline { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn read_password_file_rejects_leading_blank_then_content() {
        // "\nhunter2\n" -- trim doesn't remove the interior \n
        // but we still detect the multi-line case.
        let f = write_temp(b"\n\nhunter2\nextra\n");
        let err = read_password_file(f.path()).unwrap_err();
        assert!(matches!(err, ConfigError::PasswordFileMultiline { .. }));
    }

    #[test]
    fn read_password_file_missing_path_returns_io_error() {
        let err = read_password_file(Path::new("/no/such/path/asl-dmr-bridge-test")).unwrap_err();
        assert!(matches!(err, ConfigError::PasswordFile { .. }));
    }

    #[test]
    fn resolve_treats_empty_source_as_unset() {
        // env supplies an empty string -- should NOT count as a
        // source, so the config password wins instead of triggering
        // PasswordAmbiguous.
        let mut cfg = parse(MINIMAL).unwrap();
        let pw =
            resolve_password(&mut cfg, None, Some(secret(""))).expect("empty env not a source");
        assert_eq!(pw.expose_secret(), "pw");
    }
}
