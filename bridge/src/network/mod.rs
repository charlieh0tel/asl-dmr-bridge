pub(crate) mod brandmeister;

use crate::config::Config;

/// Network-specific behavior for DMR protocol variants.
pub(crate) trait NetworkProfile {
    /// Build the full RPTC config packet (tag + payload).
    fn config_packet(&self, config: &Config) -> Vec<u8>;
}
