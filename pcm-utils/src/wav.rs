//! Per-call PCM-to-WAV recorder used by `PttMachine` for diagnostic
//! capture.  8 kHz mono int16 LE; size fields are filled in on
//! `finalize()` (called from `Drop` on best-effort) so a partial
//! file is still a valid WAV after a crash.

use std::fs::File;
use std::io::BufWriter;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tracing::warn;

const SAMPLE_RATE: u32 = 8000;
const CHANNELS: u16 = 1;
const BITS: u16 = 16;
const FMT_CHUNK_SIZE: u32 = 16;
/// Cap on accumulated samples so 2 * samples + 36-byte header fits in
/// the WAV's u32 RIFF size.  ~268 K seconds at 8 kHz; far beyond any
/// realistic per-call recording.
const MAX_SAMPLES: u32 = (u32::MAX - 36) / 2;

pub struct WavRecorder {
    writer: BufWriter<File>,
    samples_written: u32,
    finalized: bool,
}

impl WavRecorder {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);
        // Placeholder header; sizes patched in `finalize`.
        writer.write_all(b"RIFF")?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(b"WAVE")?;
        writer.write_all(b"fmt ")?;
        writer.write_all(&FMT_CHUNK_SIZE.to_le_bytes())?;
        writer.write_all(&1u16.to_le_bytes())?; // PCM
        writer.write_all(&CHANNELS.to_le_bytes())?;
        writer.write_all(&SAMPLE_RATE.to_le_bytes())?;
        let byte_rate = SAMPLE_RATE * u32::from(CHANNELS) * u32::from(BITS) / 8;
        writer.write_all(&byte_rate.to_le_bytes())?;
        let block_align = CHANNELS * BITS / 8;
        writer.write_all(&block_align.to_le_bytes())?;
        writer.write_all(&BITS.to_le_bytes())?;
        writer.write_all(b"data")?;
        writer.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            writer,
            samples_written: 0,
            finalized: false,
        })
    }

    pub fn write(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        if self.finalized {
            return Err(std::io::Error::other("write after finalize"));
        }
        let n = u32::try_from(pcm.len())
            .map_err(|_| std::io::Error::other("pcm chunk too large for u32"))?;
        let new_count = self
            .samples_written
            .checked_add(n)
            .filter(|&c| c <= MAX_SAMPLES)
            .ok_or_else(|| std::io::Error::other("WAV size cap reached"))?;
        for &s in pcm {
            self.writer.write_all(&s.to_le_bytes())?;
        }
        self.samples_written = new_count;
        Ok(())
    }

    pub fn finalize(&mut self) -> std::io::Result<()> {
        if self.finalized {
            return Ok(());
        }
        self.finalized = true;
        self.writer.flush()?;
        let file = self.writer.get_mut();
        let data_size = self.samples_written.saturating_mul(2);
        let riff_size = data_size.saturating_add(36);
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&riff_size.to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data_size.to_le_bytes())?;
        Ok(())
    }
}

impl Drop for WavRecorder {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

/// Open a per-call diagnostic WAV in `dir` named
/// `<kind>_<unix_ms>_<stream_id>.wav`.  Returns `None` if `dir` is
/// `None` or creation fails (a warning is logged in the latter case
/// so capture is best-effort and never breaks the call path).
#[must_use]
pub fn open_call_recorder(dir: Option<&Path>, kind: &str, stream_id: u32) -> Option<WavRecorder> {
    let dir = dir?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{kind}_{now_ms}_{stream_id}.wav"));
    match WavRecorder::create(&path) {
        Ok(rec) => Some(rec),
        Err(e) => {
            warn!("open wav recorder {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::Read as _;

    use tempfile::TempDir;

    fn tmp_path(dir: &TempDir, stem: &str) -> std::path::PathBuf {
        dir.path().join(format!("{stem}.wav"))
    }

    fn read_file(path: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn header_layout_matches_canonical_8khz_mono_i16() {
        let dir = TempDir::new().unwrap();
        let path = tmp_path(&dir, "header");
        {
            let _rec = WavRecorder::create(&path).unwrap();
        }
        let bytes = read_file(&path);
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 8000);
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 16000);
        assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36);
    }

    #[test]
    fn write_then_drop_finalizes_size_fields() {
        let dir = TempDir::new().unwrap();
        let path = tmp_path(&dir, "write");
        let samples: Vec<i16> = (0..1000).map(|i| (i * 16) as i16).collect();
        {
            let mut rec = WavRecorder::create(&path).unwrap();
            rec.write(&samples[..500]).unwrap();
            rec.write(&samples[500..]).unwrap();
        }
        let bytes = read_file(&path);
        let data_size = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        let riff_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(data_size, samples.len() as u32 * 2);
        assert_eq!(riff_size, data_size + 36);
        assert_eq!(bytes.len() as u32, riff_size + 8);
        for (i, chunk) in bytes[44..].chunks_exact(2).enumerate() {
            let s = i16::from_le_bytes([chunk[0], chunk[1]]);
            assert_eq!(s, samples[i]);
        }
    }

    #[test]
    fn write_after_finalize_returns_err() {
        let dir = TempDir::new().unwrap();
        let path = tmp_path(&dir, "wfaf");
        let mut rec = WavRecorder::create(&path).unwrap();
        rec.write(&[100i16; 4]).unwrap();
        rec.finalize().unwrap();
        let err = rec.write(&[1i16]).unwrap_err();
        assert!(err.to_string().contains("write after finalize"));
        drop(rec);
        let bytes = read_file(&path);
        // Only the pre-finalize 4 samples (8 bytes) should be in data.
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(bytes.len(), 44 + 8);
    }

    #[test]
    fn write_at_sample_cap_returns_err_without_corrupting() {
        let dir = TempDir::new().unwrap();
        let path = tmp_path(&dir, "cap");
        let mut rec = WavRecorder::create(&path).unwrap();
        rec.samples_written = MAX_SAMPLES - 1;
        rec.write(&[1i16]).unwrap(); // exactly MAX_SAMPLES, ok.
        let err = rec.write(&[1i16]).unwrap_err();
        assert!(err.to_string().contains("WAV size cap"));
        assert_eq!(rec.samples_written, MAX_SAMPLES);
    }

    #[test]
    fn finalize_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = tmp_path(&dir, "idempotent");
        let mut rec = WavRecorder::create(&path).unwrap();
        rec.write(&[100i16; 4]).unwrap();
        rec.finalize().unwrap();
        rec.finalize().unwrap();
        drop(rec);
        let bytes = read_file(&path);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
    }
}
