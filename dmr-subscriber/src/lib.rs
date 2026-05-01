//! DMR subscriber-ID lookup.
//!
//! Loads a RadioID.net-style CSV (`user.csv`, ~250k rows, ~30 MB)
//! into an in-memory `HashMap<u32, Subscriber>` so callers can turn a
//! talker's on-air DMR ID into a callsign and operator info.
//!
//! Expected CSV header (case-sensitive, RadioID's published schema):
//! ```text
//! RADIO_ID,CALLSIGN,FIRST_NAME,LAST_NAME,CITY,STATE,COUNTRY
//! ```
//!
//! Trailing columns (REMARKS, etc.) are tolerated.  Each `load()`
//! call is one-shot; callers wanting periodic refresh wrap the
//! `Subscribers` in their own atomically-swappable container and
//! call `load()` again on a timer (the bridge does this when
//! `[repeater].subscriber_refresh_interval` is set).

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use dmr_types::SubscriberId;
use serde::Deserialize;
use thiserror::Error;
use tracing::debug;
use tracing::warn;

/// Threshold above which a CSV's mtime triggers a stale-file warn.
/// 30 days picks up "operator's nightly cron broke a month ago" while
/// staying clear of normal weekly maintenance gaps.
const STALE_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// One operator record from the RadioID CSV.
///
/// All fields except `dmr_id` and `callsign` may legitimately be
/// blank (privacy-conscious operators omit personal info), so they
/// land as empty strings rather than `Option<String>` -- formatting
/// callers can `is_empty()` if they care.
#[derive(Debug, Clone)]
pub struct Subscriber {
    pub dmr_id: SubscriberId,
    pub callsign: String,
    pub first_name: String,
    pub last_name: String,
    pub city: String,
    pub state: String,
    pub country: String,
}

/// Errors returned by `Subscribers::load`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error("opening subscriber CSV {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing subscriber CSV {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
}

/// Indexed subscriber database.  Cheap to query (`O(1)` lookup);
/// expensive to build (one full CSV pass), so build once at startup
/// and share via `Arc`.
#[derive(Debug, Clone, Default)]
pub struct Subscribers {
    by_id: HashMap<SubscriberId, Subscriber>,
}

impl Subscribers {
    /// Load the entire CSV into memory.  Malformed rows are skipped
    /// with a `warn!` so a single bad line doesn't fail the whole
    /// load (RadioID dumps occasionally include odd entries).  An
    /// mtime older than 30 days triggers a stale-file warn so an
    /// operator whose nightly cron silently broke notices.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let file = File::open(path).map_err(|source| LoadError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        warn_if_stale(path, &file);
        let me = Self::from_reader(file).map_err(|source| LoadError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        debug!(path = %path.display(), entries = me.len(), "loaded DMR subscribers");
        Ok(me)
    }

    /// Build from any `Read`er.  Used by `load` and by tests with
    /// inline CSV bytes.
    pub fn from_reader<R: Read>(reader: R) -> Result<Self, csv::Error> {
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(reader);
        let mut by_id = HashMap::new();
        let mut bad_rows = 0usize;
        for record in rdr.deserialize::<CsvRow>() {
            match record {
                Ok(row) => match SubscriberId::try_from(row.radio_id) {
                    Ok(id) => {
                        by_id.insert(id, row.into_subscriber(id));
                    }
                    Err(_) => {
                        bad_rows += 1;
                        if bad_rows <= 5 {
                            warn!("skipping out-of-range RADIO_ID {}", row.radio_id);
                        }
                    }
                },
                Err(e) => {
                    bad_rows += 1;
                    if bad_rows <= 5 {
                        warn!("skipping malformed subscriber row: {e}");
                    }
                }
            }
        }
        if bad_rows > 5 {
            warn!(skipped = bad_rows, "additional malformed rows skipped");
        }
        Ok(Self { by_id })
    }

    /// Look up a subscriber by on-air DMR ID.  Raw `u32` accepted for
    /// ergonomics; out-of-range values miss without erroring.
    #[must_use]
    pub fn get(&self, dmr_id: u32) -> Option<&Subscriber> {
        SubscriberId::try_from(dmr_id)
            .ok()
            .and_then(|id| self.by_id.get(&id))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

fn warn_if_stale(path: &Path, file: &File) {
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let Ok(mtime) = metadata.modified() else {
        return;
    };
    let Ok(age) = SystemTime::now().duration_since(mtime) else {
        return;
    };
    if age > STALE_AGE {
        let days = age.as_secs() / 86_400;
        warn!(
            path = %path.display(),
            age_days = days,
            "subscriber CSV is stale; refresh from radioid.net to pick up new operators"
        );
    }
}

/// CSV row matching RadioID's published schema.  `serde(default)` on
/// optional columns lets rows with blanks deserialize cleanly.
#[derive(Debug, Deserialize)]
struct CsvRow {
    #[serde(rename = "RADIO_ID")]
    radio_id: u32,
    #[serde(rename = "CALLSIGN")]
    callsign: String,
    #[serde(rename = "FIRST_NAME", default)]
    first_name: String,
    #[serde(rename = "LAST_NAME", default)]
    last_name: String,
    #[serde(rename = "CITY", default)]
    city: String,
    #[serde(rename = "STATE", default)]
    state: String,
    #[serde(rename = "COUNTRY", default)]
    country: String,
}

impl CsvRow {
    fn into_subscriber(self, dmr_id: SubscriberId) -> Subscriber {
        Subscriber {
            dmr_id,
            callsign: self.callsign,
            first_name: self.first_name,
            last_name: self.last_name,
            city: self.city,
            state: self.state,
            country: self.country,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
RADIO_ID,CALLSIGN,FIRST_NAME,LAST_NAME,CITY,STATE,COUNTRY
1234567,N0CALL,Test,User,Springfield,Illinois,United States
3151238,W6XYZ,Jane,Doe,,California,United States
5201886,GB7TST,,,Manchester,,United Kingdom
";

    #[test]
    fn load_parses_radioid_format() {
        let subs = Subscribers::from_reader(SAMPLE.as_bytes()).unwrap();
        assert_eq!(subs.len(), 3);

        let s = subs.get(1234567).expect("N0CALL present");
        assert_eq!(s.callsign, "N0CALL");
        assert_eq!(s.first_name, "Test");
        assert_eq!(s.country, "United States");
    }

    #[test]
    fn load_tolerates_blank_optional_fields() {
        let subs = Subscribers::from_reader(SAMPLE.as_bytes()).unwrap();
        let s = subs.get(5201886).expect("GB7TST present");
        assert_eq!(s.callsign, "GB7TST");
        assert!(s.first_name.is_empty());
        assert!(s.last_name.is_empty());
        assert_eq!(s.country, "United Kingdom");
    }

    #[test]
    fn lookup_miss_returns_none() {
        let subs = Subscribers::from_reader(SAMPLE.as_bytes()).unwrap();
        assert!(subs.get(9999999).is_none());
    }

    #[test]
    fn empty_csv_yields_empty_db() {
        let subs = Subscribers::from_reader("RADIO_ID,CALLSIGN\n".as_bytes()).unwrap();
        assert!(subs.is_empty());
        assert_eq!(subs.len(), 0);
    }

    #[test]
    fn extra_trailing_columns_are_tolerated() {
        // RadioID adds REMARKS / other trailing columns periodically;
        // the loader must not break when columns extend.
        let csv = "\
RADIO_ID,CALLSIGN,FIRST_NAME,LAST_NAME,CITY,STATE,COUNTRY,REMARKS,EXTRA
1,N0CALL,Test,User,City,ST,US,note,more
";
        let subs = Subscribers::from_reader(csv.as_bytes()).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs.get(1).unwrap().callsign, "N0CALL");
    }
}
