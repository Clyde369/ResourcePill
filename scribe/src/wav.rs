//! Streaming 16 kHz mono 16-bit WAV writer (sizes patched in on finish).

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

pub struct Writer {
    out: BufWriter<File>,
    samples: u32,
}

impl Writer {
    pub fn create(path: &Path) -> Option<Writer> {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let mut out = BufWriter::new(File::create(path).ok()?);
        out.write_all(&header(0)).ok()?;
        Some(Writer { out, samples: 0 })
    }

    pub fn write(&mut self, s: &[f32]) {
        for &x in s {
            let v = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
            if self.out.write_all(&v.to_le_bytes()).is_err() {
                return;
            }
        }
        self.samples += s.len() as u32;
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        self.out.flush()?;
        let mut f = self.out.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.samples))?;
        Ok(())
    }
}

fn header(samples: u32) -> [u8; 44] {
    let data = samples * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&16_000u32.to_le_bytes());
    h[28..32].copy_from_slice(&32_000u32.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}
