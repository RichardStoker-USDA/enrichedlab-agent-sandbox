//! The file's container: a ZIP of deflated entries with one fixed date, no extra fields, comments or data descriptors,
//! the parts in the order they are added.
use std::io::Write as _;

use flate2::{Compression, Crc, write::DeflateEncoder};
use time::OffsetDateTime;

use super::{MAX_FILE_BYTES, WriteError, invalid, too_large};

/// A ZIP of deflated entries with one fixed date, no extra fields, comments or data descriptors.
pub(super) struct Zip {
    out: Vec<u8>,
    central: Vec<u8>,
    entries: u16,
    time: u16,
    date: u16,
}

impl Zip {
    pub(super) fn new(at: OffsetDateTime) -> Self {
        // DOS dates run from 1980 to 2107 with two-second steps; the core properties keep the exact time.
        let year = at.year().clamp(1980, 2107);
        let (time, date) = if year == at.year() {
            (
                (u16::from(at.hour()) << 11)
                    | (u16::from(at.minute()) << 5)
                    | u16::from(at.second() / 2),
                (((year - 1980) as u16) << 9)
                    | (u16::from(u8::from(at.month())) << 5)
                    | u16::from(at.day()),
            )
        } else if year == 1980 {
            (0, (1 << 5) | 1)
        } else {
            ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31)
        };
        Zip {
            out: Vec::new(),
            central: Vec::new(),
            entries: 0,
            time,
            date,
        }
    }

    pub(super) fn add(&mut self, name: &str, data: &[u8]) -> Result<(), WriteError> {
        let mut crc = Crc::new();
        crc.update(data);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(data).map_err(|_| too_large())?;
        let compressed = encoder.finish().map_err(|_| too_large())?;
        let offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let (stored, size) = (
            u32::try_from(compressed.len()).map_err(|_| too_large())?,
            u32::try_from(data.len()).map_err(|_| too_large())?,
        );
        let name_length = u16::try_from(name.len()).map_err(|_| invalid())?;
        let (time, date) = (self.time, self.date);
        // Version needed, flags, method (deflate), time, date, CRC-32, sizes, name and extra field lengths.
        let fixed = |buffer: &mut Vec<u8>| {
            buffer.extend_from_slice(&20u16.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            buffer.extend_from_slice(&8u16.to_le_bytes());
            buffer.extend_from_slice(&time.to_le_bytes());
            buffer.extend_from_slice(&date.to_le_bytes());
            buffer.extend_from_slice(&crc.sum().to_le_bytes());
            buffer.extend_from_slice(&stored.to_le_bytes());
            buffer.extend_from_slice(&size.to_le_bytes());
            buffer.extend_from_slice(&name_length.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
        };
        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        fixed(&mut self.out);
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(&compressed);
        self.central
            .extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes());
        fixed(&mut self.central);
        // Comment length, disk, internal and external attributes, then the local header's offset.
        self.central.extend_from_slice(&[0; 10]);
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.entries = self.entries.checked_add(1).ok_or_else(too_large)?;
        if self.out.len() + self.central.len() > MAX_FILE_BYTES {
            return Err(too_large());
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Vec<u8>, WriteError> {
        let offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let size = u32::try_from(self.central.len()).map_err(|_| too_large())?;
        self.out.append(&mut self.central);
        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.extend_from_slice(&self.entries.to_le_bytes());
        self.out.extend_from_slice(&self.entries.to_le_bytes());
        self.out.extend_from_slice(&size.to_le_bytes());
        self.out.extend_from_slice(&offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        Ok(self.out)
    }
}
