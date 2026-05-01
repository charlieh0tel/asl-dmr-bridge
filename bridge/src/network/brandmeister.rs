use crate::config::Config;
use crate::network::NetworkProfile;
use dmr_types::REPEATER_ID_WIRE_LEN;
use dmr_types::Slot;

const TAG_RPTC: &[u8] = b"RPTC";

// BM validates specific RPTC fields (empirically determined):
//   software_id:  must match /YYYYMMDD_*/
//   package_id:   must start with "MMDVM"
//   slots:        '1' or '2', derived from [dmr] slot (single-slot daemon)
//   longitude:    must include sign prefix
//   rx_freq/tx_freq: must be non-zero (required by Frequency type at
//                    deserialization time)
// Description, location, url, power, color_code, height are unchecked.
const SOFTWARE_ID: &str = "20260412_asl-dmr-bridge";
const PACKAGE_ID: &str = "MMDVM_asl-dmr-bridge";
/// Latitude bounds that fit the LATITUDE_WIDTH field (`+NN.NNNN`).
const MAX_LATITUDE: f64 = 89.9999;
/// Longitude bounds that fit the LONGITUDE_WIDTH field (`+NNN.NNNN`).
const MAX_LONGITUDE: f64 = 179.9999;
const DEFAULT_LOCATION: &str = "";
const DEFAULT_DESCRIPTION: &str = "asl-dmr-bridge";

/// RPTC config field widths per DMRGateway getConfig() sprintf format:
/// `%8.8s%9.9s%9.9s%2.2s%2.2s%+08.4f%+09.4f%03d%-20.20s%-19.19s%c%-124.124s%40.40s%40.40s`
const CALLSIGN_WIDTH: usize = 8;
const RX_FREQ_WIDTH: usize = 9;
const TX_FREQ_WIDTH: usize = 9;
const TX_POWER_WIDTH: usize = 2;
const COLOR_CODE_WIDTH: usize = 2;
const LATITUDE_WIDTH: usize = 8;
const LONGITUDE_WIDTH: usize = 9;
const HEIGHT_WIDTH: usize = 3;
const LOCATION_WIDTH: usize = 20;
const DESCRIPTION_WIDTH: usize = 19;
const SLOTS_WIDTH: usize = 1;
const URL_WIDTH: usize = 124;
const SOFTWARE_ID_WIDTH: usize = 40;
const PACKAGE_ID_WIDTH: usize = 40;

const CONFIG_STRING_LEN: usize = CALLSIGN_WIDTH
    + RX_FREQ_WIDTH
    + TX_FREQ_WIDTH
    + TX_POWER_WIDTH
    + COLOR_CODE_WIDTH
    + LATITUDE_WIDTH
    + LONGITUDE_WIDTH
    + HEIGHT_WIDTH
    + LOCATION_WIDTH
    + DESCRIPTION_WIDTH
    + SLOTS_WIDTH
    + URL_WIDTH
    + SOFTWARE_ID_WIDTH
    + PACKAGE_ID_WIDTH;

pub(crate) struct Brandmeister;

/// Return `value` if non-empty, otherwise `default`.
fn or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

/// Format a string field: left-aligned, space-padded, byte-truncated
/// to width on a UTF-8 boundary.
fn fmt_str(value: &str, width: usize) -> String {
    // Find the largest char-boundary byte offset <= width.
    let cut = value.len().min(width);
    let cut = (0..=cut)
        .rev()
        .find(|&i| value.is_char_boundary(i))
        .unwrap_or(0);
    let mut s = String::with_capacity(width);
    s.push_str(&value[..cut]);
    // ASCII space padding is always a valid char boundary.
    for _ in s.len()..width {
        s.push(' ');
    }
    debug_assert_eq!(s.len(), width);
    s
}

/// Format a lat/lon field with sign, zero-padded to width, 4 decimal
/// places.  Clamps out-of-range values to what fits.
fn fmt_latlon(value: f64, width: usize, limit: f64) -> String {
    let v = value.clamp(-limit, limit);
    let s = format!("{v:+0width$.4}");
    debug_assert_eq!(
        s.len(),
        width,
        "lat/lon width mismatch: expected {width}, got {}",
        s.len()
    );
    s
}

impl NetworkProfile for Brandmeister {
    /// Build RPTC config packet.
    ///
    /// Format matches DMRGateway's getConfig() sprintf.
    fn config_packet(&self, config: &Config) -> Vec<u8> {
        use std::fmt::Write as _;

        let r = &config.repeater;
        let cc = r.color_code.value();
        let height = r.height.unwrap_or(0).min(999);

        let mut cfg = String::with_capacity(CONFIG_STRING_LEN);
        cfg.push_str(&fmt_str(r.callsign.as_str(), CALLSIGN_WIDTH));
        cfg.push_str(&fmt_str(&r.rx_freq.as_rptc_digits(), RX_FREQ_WIDTH));
        cfg.push_str(&fmt_str(&r.tx_freq.as_rptc_digits(), TX_FREQ_WIDTH));
        cfg.push_str(&fmt_str(or_default(&r.tx_power, "01"), TX_POWER_WIDTH));
        // color_code (u8) and height (u32) format to fixed-width
        // ASCII via the std width specifier; no fmt_str padding pass
        // needed.  write! into String never fails.
        write!(cfg, "{cc:0COLOR_CODE_WIDTH$}").expect("String::write_str is infallible");
        cfg.push_str(&fmt_latlon(
            r.latitude.unwrap_or(0.0),
            LATITUDE_WIDTH,
            MAX_LATITUDE,
        ));
        cfg.push_str(&fmt_latlon(
            r.longitude.unwrap_or(0.0),
            LONGITUDE_WIDTH,
            MAX_LONGITUDE,
        ));
        write!(cfg, "{height:0HEIGHT_WIDTH$}").expect("String::write_str is infallible");
        cfg.push_str(&fmt_str(
            or_default(&r.location, DEFAULT_LOCATION),
            LOCATION_WIDTH,
        ));
        cfg.push_str(&fmt_str(
            or_default(&r.description, DEFAULT_DESCRIPTION),
            DESCRIPTION_WIDTH,
        ));
        cfg.push(match config.dmr.slot {
            Slot::One => '1',
            Slot::Two => '2',
        });
        cfg.push_str(&fmt_str(&r.url, URL_WIDTH));
        cfg.push_str(&fmt_str(SOFTWARE_ID, SOFTWARE_ID_WIDTH));
        cfg.push_str(&fmt_str(PACKAGE_ID, PACKAGE_ID_WIDTH));

        assert_eq!(cfg.len(), CONFIG_STRING_LEN);

        let mut pkt = Vec::with_capacity(TAG_RPTC.len() + REPEATER_ID_WIRE_LEN + CONFIG_STRING_LEN);
        pkt.extend_from_slice(TAG_RPTC);
        pkt.extend_from_slice(&r.dmr_id.to_be_bytes());
        pkt.extend_from_slice(cfg.as_bytes());

        pkt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        toml::from_str(
            r#"
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
            backend = "thumbdv"
            serial_port = "/dev/ttyUSB0"

            [dmr]
            slot = 1
            talkgroup = 2
            call_type = "group"
            hang_time = "500ms"
            stream_timeout = "5s"

            [network]
            profile = "brandmeister"
            host = "3104.master.brandmeister.network"
            port = 62031
            password = "test"
            keepalive_interval = "5s"
            keepalive_missed_limit = 3
            "#,
        )
        .unwrap()
    }

    #[test]
    fn config_packet_length() {
        let pkt = Brandmeister.config_packet(&test_config());
        assert_eq!(
            pkt.len(),
            TAG_RPTC.len() + REPEATER_ID_WIRE_LEN + CONFIG_STRING_LEN
        );
    }

    #[test]
    fn config_packet_tag() {
        let pkt = Brandmeister.config_packet(&test_config());
        assert_eq!(&pkt[..4], TAG_RPTC);
    }

    #[test]
    fn config_packet_repeater_id() {
        let pkt = Brandmeister.config_packet(&test_config());
        assert_eq!(&pkt[4..8], &[0x00, 0x12, 0xD6, 0x87]);
    }

    #[test]
    fn config_packet_callsign() {
        let pkt = Brandmeister.config_packet(&test_config());
        let start = TAG_RPTC.len() + REPEATER_ID_WIRE_LEN;
        assert_eq!(&pkt[start..start + CALLSIGN_WIDTH], b"N0CALL  ");
    }

    fn full_config() -> Config {
        toml::from_str(
            r#"
            [repeater]
            callsign = "N0CALL"
            dmr_id = 1234567
            src_id = 1234567
            rx_freq = 434000000
            tx_freq = 439000000
            tx_power = "25"
            color_code = 7
            latitude = 0.0
            longitude = 0.0
            height = 50
            location = "Anywhere"
            description = "Test bridge"
            url = "http://example.org"

            [usrp]
            local_host = "127.0.0.1"
            local_port = 34001
            remote_host = "127.0.0.1"
            remote_port = 34002

            [vocoder]
            backend = "thumbdv"
            serial_port = "/dev/ttyUSB0"

            [dmr]
            slot = 1
            talkgroup = 91
            call_type = "group"
            hang_time = "500ms"
            stream_timeout = "5s"

            [network]
            profile = "brandmeister"
            host = "test.master.brandmeister.network"
            port = 62031
            password = "test"
            keepalive_interval = "5s"
            keepalive_missed_limit = 3
            "#,
        )
        .unwrap()
    }

    #[test]
    fn config_packet_full_layout() {
        // Locks the byte-exact RPTC field layout: any reorder, width
        // change, or per-field formatting drift will fail this test.
        // Each ASCII span below is a single field (callsign, rx_freq,
        // tx_freq, tx_power, color_code, lat, lon, height, location,
        // description, slots, url, software_id, package_id).
        let pkt = Brandmeister.config_packet(&full_config());
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"RPTC");
        expected.extend_from_slice(&[0x00, 0x12, 0xD6, 0x87]); // dmr_id 1234567 BE
        expected.extend_from_slice(b"N0CALL  "); // callsign (8)
        expected.extend_from_slice(b"434000000"); // rx_freq (9)
        expected.extend_from_slice(b"439000000"); // tx_freq (9)
        expected.extend_from_slice(b"25"); // tx_power (2)
        expected.extend_from_slice(b"07"); // color_code (2)
        expected.extend_from_slice(b"+00.0000"); // latitude (8)
        expected.extend_from_slice(b"+000.0000"); // longitude (9)
        expected.extend_from_slice(b"050"); // height (3)
        expected.extend_from_slice(b"Anywhere            "); // location (20)
        expected.extend_from_slice(b"Test bridge        "); // description (19)
        expected.extend_from_slice(b"1"); // slots (1)
        let mut url = b"http://example.org".to_vec();
        url.resize(124, b' ');
        expected.extend_from_slice(&url); // url (124)
        let mut sw = b"20260412_asl-dmr-bridge".to_vec();
        sw.resize(40, b' ');
        expected.extend_from_slice(&sw); // software_id (40)
        let mut pkg = b"MMDVM_asl-dmr-bridge".to_vec();
        pkg.resize(40, b' ');
        expected.extend_from_slice(&pkg); // package_id (40)
        assert_eq!(
            pkt,
            expected,
            "RPTC byte layout drift: got {} expected {}",
            String::from_utf8_lossy(&pkt),
            String::from_utf8_lossy(&expected)
        );
    }

    #[test]
    fn config_packet_lat_lon() {
        let pkt = Brandmeister.config_packet(&test_config());
        let lat_start = TAG_RPTC.len()
            + REPEATER_ID_WIRE_LEN
            + CALLSIGN_WIDTH
            + RX_FREQ_WIDTH
            + TX_FREQ_WIDTH
            + TX_POWER_WIDTH
            + COLOR_CODE_WIDTH;
        let lat = std::str::from_utf8(&pkt[lat_start..lat_start + LATITUDE_WIDTH]).unwrap();
        let lon = std::str::from_utf8(
            &pkt[lat_start + LATITUDE_WIDTH..lat_start + LATITUDE_WIDTH + LONGITUDE_WIDTH],
        )
        .unwrap();
        assert_eq!(lat, "+00.0000");
        assert_eq!(lon, "+000.0000");
    }

    // --- fmt_str ---

    #[test]
    fn fmt_str_pads_short_input() {
        assert_eq!(fmt_str("ab", 5), "ab   ");
    }

    #[test]
    fn fmt_str_passes_exact_width() {
        assert_eq!(fmt_str("abcde", 5), "abcde");
    }

    #[test]
    fn fmt_str_truncates_long_input() {
        assert_eq!(fmt_str("abcdefgh", 5), "abcde");
    }

    #[test]
    fn fmt_str_empty_input_pads_full_width() {
        assert_eq!(fmt_str("", 4), "    ");
    }

    #[test]
    fn fmt_str_zero_width_returns_empty() {
        assert_eq!(fmt_str("anything", 0), "");
    }

    #[test]
    fn fmt_str_truncates_at_utf8_boundary() {
        // "ab" + 'é' (U+00E9, two bytes 0xC3 0xA9) + "f"
        // = 5 bytes total, char at byte index 2 spans bytes [2..4].
        // Truncating to width 3 must drop the multibyte 'é' rather
        // than splitting it -- the result is "ab " (last byte padded).
        let s = "abéf";
        assert_eq!(fmt_str(s, 3), "ab ");
    }

    #[test]
    fn fmt_str_truncates_inside_multibyte_char_pads_remainder() {
        // 4-byte char at the start: "🎵" (U+1F3B5).  Truncating to
        // width 2 must yield two ASCII spaces, never bytes 0..2 of
        // the codepoint.
        let s = "\u{1F3B5}xyz";
        let out = fmt_str(s, 2);
        assert_eq!(out, "  ");
        assert!(out.is_ascii(), "must not emit partial UTF-8 bytes");
    }

    #[test]
    fn fmt_str_keeps_full_multibyte_char_when_room_exists() {
        // "abéf" is 5 bytes (a, b, 0xC3, 0xA9, f).  width=6 should
        // keep the whole thing and pad with a trailing ASCII space.
        let s = "abéf";
        let out = fmt_str(s, 6);
        assert_eq!(out, "abéf ");
        assert_eq!(out.len(), 6);
    }

    // --- fmt_latlon ---

    #[test]
    fn fmt_latlon_zero_formats_with_sign() {
        // Format: "+NN.NNNN" (latitude, width 8) -- always emits the
        // sign character so the field is fixed-width.
        assert_eq!(fmt_latlon(0.0, LATITUDE_WIDTH, MAX_LATITUDE), "+00.0000");
    }

    #[test]
    fn fmt_latlon_positive_in_range() {
        assert_eq!(
            fmt_latlon(37.7749, LATITUDE_WIDTH, MAX_LATITUDE),
            "+37.7749"
        );
    }

    #[test]
    fn fmt_latlon_negative_in_range() {
        assert_eq!(
            fmt_latlon(-37.7749, LATITUDE_WIDTH, MAX_LATITUDE),
            "-37.7749"
        );
    }

    #[test]
    fn fmt_latlon_clamps_above_limit() {
        // 95.0 > MAX_LATITUDE (89.9999); must clamp.
        let out = fmt_latlon(95.0, LATITUDE_WIDTH, MAX_LATITUDE);
        assert_eq!(out, "+89.9999");
        assert_eq!(out.len(), LATITUDE_WIDTH);
    }

    #[test]
    fn fmt_latlon_clamps_below_negative_limit() {
        let out = fmt_latlon(-100.0, LATITUDE_WIDTH, MAX_LATITUDE);
        assert_eq!(out, "-89.9999");
        assert_eq!(out.len(), LATITUDE_WIDTH);
    }

    #[test]
    fn fmt_latlon_at_positive_boundary_fits_field() {
        // Boundary value: must serialize without overflowing the
        // fixed-width field.
        let out = fmt_latlon(MAX_LATITUDE, LATITUDE_WIDTH, MAX_LATITUDE);
        assert_eq!(out.len(), LATITUDE_WIDTH);
        assert_eq!(out, "+89.9999");
    }

    #[test]
    fn fmt_latlon_at_negative_boundary_fits_field() {
        let out = fmt_latlon(-MAX_LATITUDE, LATITUDE_WIDTH, MAX_LATITUDE);
        assert_eq!(out.len(), LATITUDE_WIDTH);
        assert_eq!(out, "-89.9999");
    }

    #[test]
    fn fmt_latlon_longitude_field_is_one_byte_wider() {
        // Longitudes need an extra digit (+NNN.NNNN).  Boundary
        // values must still fit LONGITUDE_WIDTH bytes.
        let out = fmt_latlon(MAX_LONGITUDE, LONGITUDE_WIDTH, MAX_LONGITUDE);
        assert_eq!(out.len(), LONGITUDE_WIDTH);
        assert_eq!(out, "+179.9999");
        let neg = fmt_latlon(-MAX_LONGITUDE, LONGITUDE_WIDTH, MAX_LONGITUDE);
        assert_eq!(neg, "-179.9999");
    }
}
