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

    /// `vocoder.gain_in_db` / `gain_out_db` outside the chip's
    /// supported range.  Reject at load time rather than silently
    /// clamping in `dv3000::build_gain`.
    #[error("vocoder.{field} = {value} dB outside supported range [{min}, {max}]")]
    GainOutOfRange {
        field: &'static str,
        value: i8,
        min: i8,
        max: i8,
    },

    #[error(
        "no BM password supplied (set [network].password in config, \
         BM_BRIDGE_PASSWORD env var, or --password-file)"
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
    BmApiKeyAmbiguous,

    #[error(
        "brandmeister_api static_talkgroups_ts{slot} declared but no API key supplied \
         (set api_key, api_key_file, or BRANDMEISTER_API_KEY env var)"
    )]
    BmApiKeyMissingForStatics { slot: u8 },

    #[error("reading brandmeister_api.api_key_file {path}")]
    BmApiKeyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("brandmeister_api.api_key_file {path} has multiple lines; expected single-line JWT")]
    BmApiKeyFileMultiline { path: PathBuf },
}

/// Top-level configuration, mirrors DESIGN.md configuration schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) repeater: RepeaterConfig,
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
    /// Optional automatic gain control on the USRP-tx (digital ->
    /// analog) path.  Off by default; existing setups using
    /// `vocoder.gain_out_db` see no behavior change.
    #[serde(default)]
    pub(crate) agc: AgcConfig,
}

/// AGC parameters with sensible defaults; `enabled = false` skips
/// processing entirely so the path stays bit-exact when AGC is off.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgcConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default = "default_agc_target_dbfs")]
    pub(crate) target_dbfs: f32,
    #[serde(default = "default_agc_attack", with = "humantime_serde")]
    pub(crate) attack: Duration,
    #[serde(default = "default_agc_release", with = "humantime_serde")]
    pub(crate) release: Duration,
    #[serde(default = "default_agc_max_gain_db")]
    pub(crate) max_gain_db: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_dbfs: default_agc_target_dbfs(),
            attack: default_agc_attack(),
            release: default_agc_release(),
            max_gain_db: default_agc_max_gain_db(),
        }
    }
}

fn default_agc_target_dbfs() -> f32 {
    -6.0
}
fn default_agc_attack() -> Duration {
    Duration::from_millis(10)
}
fn default_agc_release() -> Duration {
    Duration::from_millis(200)
}
fn default_agc_max_gain_db() -> f32 {
    30.0
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
pub(crate) struct RepeaterConfig {
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
    Thumbdv,
    Ambeserver,
    Mbelib,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VocoderConfig {
    pub(crate) backend: VocoderBackend,
    /// Serial port path for ThumbDV (e.g. "/dev/ttyUSB0").
    pub(crate) serial_port: Option<String>,
    /// Serial baud rate for ThumbDV (default 460800).
    pub(crate) serial_baud: Option<u32>,
    /// AMBEserver host (when backend = "ambeserver").
    pub(crate) host: Option<String>,
    /// AMBEserver port (when backend = "ambeserver", default 2460).
    pub(crate) port: Option<u16>,
    /// DV3000 chip input (encode) gain in dB, -90..=90.  Default 0.
    /// Ignored by the mbelib backend.
    pub(crate) gain_in_db: Option<i8>,
    /// DV3000 chip output (decode) gain in dB, -90..=90.  Default 0.
    /// Ignored by the mbelib backend.
    pub(crate) gain_out_db: Option<i8>,
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
    /// Minimum time to keep a DMR call open after a USRP unkey.  A
    /// re-key within this window stays in the same call (smooths over
    /// brief PTT taps and momentary unkeys).  Default `0` preserves
    /// the immediate-terminator behavior; suggested value `2.5s` for
    /// human voice traffic.  Must be less than `stream_timeout`.
    #[serde(with = "humantime_serde", default = "default_min_tx_hang")]
    pub(crate) min_tx_hang: Duration,
}

fn default_tx_timeout() -> Duration {
    Duration::from_secs(180)
}

fn default_min_tx_hang() -> Duration {
    Duration::ZERO
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
    /// One of three password sources (the others being
    /// `--password-file` and `BM_BRIDGE_PASSWORD`).  Pass through
    /// `resolve_password` at startup; that function takes ownership
    /// and returns the resolved secret directly, so this field is
    /// `None` for the rest of the process lifetime.  `SecretString`
    /// keeps the value out of `Debug` and zeroizes on drop.
    #[serde(default)]
    pub(crate) password: Option<SecretString>,
    #[serde(with = "humantime_serde")]
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_missed_limit: u32,
}

/// DV3000 gain packet range.  Matches `ambe::dv3000::GAIN_MIN_DB` /
/// `GAIN_MAX_DB`; duplicated here because the ambe crate keeps them
/// `pub(crate)`, and the config layer rejects out-of-range values
/// before they reach the chip rather than silently clamping.
const GAIN_MIN_DB: i8 = -90;
const GAIN_MAX_DB: i8 = 90;

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
        check_gain("gain_in_db", self.vocoder.gain_in_db)?;
        check_gain("gain_out_db", self.vocoder.gain_out_db)?;
        if self.network.keepalive_interval.is_zero() {
            return Err(ConfigError::KeepaliveIntervalZero);
        }
        Ok(())
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

/// Resolve the BM password from any of three sources, in this
/// priority order: `--password-file`, `BM_BRIDGE_PASSWORD` env var,
/// `[network].password` in config.  Exactly one source must supply
/// a non-empty value; zero is `PasswordMissing`, more than one is
/// `PasswordAmbiguous` (catches operator confusion).  Returns the
/// resolved secret directly so the caller holds a `SecretString`
/// rather than an `Option<SecretString>` field invariant.
pub(crate) fn resolve_password(
    config: &mut Config,
    file_source: Option<SecretString>,
    env_source: Option<SecretString>,
) -> Result<SecretString, ConfigError> {
    fn non_empty(s: SecretString) -> Option<SecretString> {
        if s.expose_secret().is_empty() {
            None
        } else {
            Some(s)
        }
    }
    let candidates: Vec<(&'static str, SecretString)> = [
        ("--password-file", file_source),
        ("BM_BRIDGE_PASSWORD", env_source),
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
        SecretFileError::Io(source) => ConfigError::BmApiKeyFile {
            path: path.to_path_buf(),
            source,
        },
        SecretFileError::Multiline => ConfigError::BmApiKeyFileMultiline {
            path: path.to_path_buf(),
        },
    })
}

/// Resolve the Brandmeister API key from up to three sources:
/// `BRANDMEISTER_API_KEY` env var, `[brandmeister_api].api_key_file`,
/// or `[brandmeister_api].api_key`.  At most one may be set
/// (`api_key` and `api_key_file` are checked at config-validate time;
/// the env var is the third candidate here).  After this call, if a
/// `[brandmeister_api]` section exists, its `api_key` field is
/// `Some(resolved)` if any source supplied a non-empty key.  The
/// round-trip lives on for now because `bm_provision` reads the key
/// straight from the config -- unlike the BM password which has a
/// single consumer in main.rs.
///
/// Unlike the BM password, the API key is *optional*: anonymous
/// reads (peer profile log) work without it, so missing key just
/// means "no write-path provisioning" rather than a startup error.
/// The caller validates "key required for declared statics".
pub(crate) fn resolve_api_key(
    config: &mut Config,
    env_source: Option<SecretString>,
) -> Result<(), ConfigError> {
    let Some(api_cfg) = config.brandmeister_api.as_mut() else {
        return Ok(());
    };
    fn non_empty(s: SecretString) -> Option<SecretString> {
        if s.expose_secret().is_empty() {
            None
        } else {
            Some(s)
        }
    }
    let file_source = match api_cfg.api_key_file.take() {
        Some(path) => read_api_key_file(&path)?,
        None => None,
    };
    let candidates: Vec<(&'static str, SecretString)> = [
        ("BRANDMEISTER_API_KEY", env_source),
        ("brandmeister_api.api_key_file", file_source),
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
                return Err(ConfigError::BmApiKeyMissingForStatics { slot: 1 });
            }
            if api_cfg.static_talkgroups_ts2.is_some() {
                return Err(ConfigError::BmApiKeyMissingForStatics { slot: 2 });
            }
            Ok(())
        }
        1 => {
            let (source, secret) = candidates
                .into_iter()
                .next()
                .expect("candidates.len() == 1 by match arm");
            tracing::info!(source, "loaded Brandmeister API key");
            api_cfg.api_key = Some(secret);
            Ok(())
        }
        _ => Err(ConfigError::BmApiKeyAmbiguous),
    }
}

fn check_gain(field: &'static str, value: Option<i8>) -> Result<(), ConfigError> {
    if let Some(v) = value
        && !(GAIN_MIN_DB..=GAIN_MAX_DB).contains(&v)
    {
        return Err(ConfigError::GainOutOfRange {
            field,
            value: v,
            min: GAIN_MIN_DB,
            max: GAIN_MAX_DB,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid config text; tests tweak one field at a time.
    const MINIMAL: &str = r#"
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
        assert_eq!(cfg.repeater.callsign.as_str(), "N0CALL");
        assert_eq!(cfg.repeater.dmr_id.as_u32(), 1234567);
        assert_eq!(cfg.dmr.slot, Slot::One);
        assert_eq!(cfg.dmr.talkgroup.as_u32(), 9);
        assert!(matches!(cfg.dmr.gateway, GatewayMode::Both));
        assert!(cfg.vocoder.gain_in_db.is_none());
    }

    #[test]
    fn malformed_toml_returns_parse_error() {
        let bad = "[repeater]\ncallsign = ";
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
    fn gain_in_range_accepted() {
        let text = MINIMAL.replace(
            "backend = \"mbelib\"",
            "backend = \"mbelib\"\ngain_in_db = -3\ngain_out_db = 6",
        );
        let cfg = parse(&text).expect("in-range gain accepted");
        assert_eq!(cfg.vocoder.gain_in_db, Some(-3));
        assert_eq!(cfg.vocoder.gain_out_db, Some(6));
    }

    #[test]
    fn gain_below_min_rejected() {
        let text = MINIMAL.replace(
            "backend = \"mbelib\"",
            "backend = \"mbelib\"\ngain_in_db = -100",
        );
        let err = parse(&text).expect_err("below-min gain rejected");
        assert!(
            matches!(
                err,
                ConfigError::GainOutOfRange {
                    field: "gain_in_db",
                    value: -100,
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn gain_above_max_rejected() {
        let text = MINIMAL.replace(
            "backend = \"mbelib\"",
            "backend = \"mbelib\"\ngain_out_db = 100",
        );
        let err = parse(&text).expect_err("above-max gain rejected");
        assert!(
            matches!(
                err,
                ConfigError::GainOutOfRange {
                    field: "gain_out_db",
                    value: 100,
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn gain_at_boundaries_accepted() {
        let text = MINIMAL.replace(
            "backend = \"mbelib\"",
            "backend = \"mbelib\"\ngain_in_db = -90\ngain_out_db = 90",
        );
        let cfg = parse(&text).expect("boundary values accepted");
        assert_eq!(cfg.vocoder.gain_in_db, Some(-90));
        assert_eq!(cfg.vocoder.gain_out_db, Some(90));
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
        let text = MINIMAL.replace("backend = \"mbelib\"", "backend = \"bogus\"");
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
