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

const SAMPLE_RATE: u32 = 8000;
const CHANNELS: u16 = 1;
const BITS: u16 = 16;
const FMT_CHUNK_SIZE: u32 = 16;

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
        for &s in pcm {
            self.writer.write_all(&s.to_le_bytes())?;
        }
        self.samples_written = self.samples_written.saturating_add(pcm.len() as u32);
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
