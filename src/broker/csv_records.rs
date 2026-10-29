//! CSV and TSV records read with fixed room. The csv crate grows a record's buffers to fit whatever it reads, so a
//! header of sixteen million empty fields cost about 512 MiB before a column limit could refuse it. These records are
//! read by the same reader core (`csv-core`) with the csv crate's defaults: RFC 4180 quoting with doubled quotes,
//! `\r`, `\n` or `\r\n` record ends, blank lines skipped, and a leading UTF-8 byte-order mark dropped. A record with
//! more fields or more text than the caller allows is refused when its room fills, before more is allocated.
use std::io::{self, BufRead};

/// What `Records::read_record` read.
#[derive(Debug, PartialEq, Eq)]
pub enum Record {
    /// A record with this many fields, which `Records::fields` gives.
    Fields(usize),
    /// A record with more fields than the reader allows.
    TooManyFields,
    /// A record with more unquoted text than the reader allows.
    TooLong,
}

pub struct Records<R> {
    input: R,
    core: csv_core::Reader,
    output: Vec<u8>,
    ends: Vec<usize>,
    max_bytes: usize,
    count: usize,
    consumed: u64,
    start: u64,
}

impl<R: BufRead> Records<R> {
    /// Records of at most `max_fields` fields and `max_bytes` bytes of unquoted text each.
    pub fn new(input: R, delimiter: u8, max_fields: usize, max_bytes: usize) -> Self {
        Records {
            input,
            core: csv_core::ReaderBuilder::new().delimiter(delimiter).build(),
            // The text buffer starts small and doubles up to its limit; one more slot than the limits allow lets a
            // record that fills them be told apart from one that passes them.
            output: vec![0; max_bytes.min(64 * 1024) + 1],
            ends: vec![0; max_fields + 1],
            max_bytes,
            count: 0,
            consumed: 0,
            start: 0,
        }
    }

    /// The next record, or none at the end of the input. A refused record is not finished: the caller stops reading.
    pub fn read_record(&mut self) -> io::Result<Option<Record>> {
        use csv_core::ReadRecordResult as Read;
        self.start = self.consumed;
        let (mut written, mut ended) = (0, 0);
        loop {
            let input = self.input.fill_buf()?;
            let (result, read, wrote, ends) =
                self.core
                    .read_record(input, &mut self.output[written..], &mut self.ends[ended..]);
            self.input.consume(read);
            self.consumed += read as u64;
            written += wrote;
            ended += ends;
            match result {
                Read::InputEmpty => {}
                Read::Record if ended == self.ends.len() => return Ok(Some(Record::TooManyFields)),
                // Text that fills the buffer just as the input ends is finished without the reader running out of room.
                Read::Record if written > self.max_bytes => return Ok(Some(Record::TooLong)),
                Read::Record => {
                    self.count = ended;
                    return Ok(Some(Record::Fields(ended)));
                }
                Read::OutputEndsFull => return Ok(Some(Record::TooManyFields)),
                Read::OutputFull if self.output.len() > self.max_bytes => {
                    return Ok(Some(Record::TooLong));
                }
                Read::OutputFull => {
                    let grown = (self.output.len() * 2).min(self.max_bytes + 1);
                    self.output.resize(grown, 0);
                }
                Read::End => return Ok(None),
            }
        }
    }

    /// The fields of the record `read_record` last answered with `Fields`.
    pub fn fields(&self) -> impl Iterator<Item = &[u8]> {
        let ends = &self.ends[..self.count];
        ends.iter().enumerate().map(move |(at, &end)| {
            let from = if at == 0 { 0 } else { ends[at - 1] };
            &self.output[from..end]
        })
    }

    /// The byte offset, from the start of the input, at which the last record's reading began. Blank lines before a
    /// record count toward it.
    pub fn start(&self) -> u64 {
        self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(
        text: &[u8],
        delimiter: u8,
        max_fields: usize,
        max_bytes: usize,
    ) -> Vec<Result<Vec<Vec<u8>>, Record>> {
        let mut records = Records::new(text, delimiter, max_fields, max_bytes);
        let mut out = Vec::new();
        while let Some(record) = records.read_record().unwrap() {
            match record {
                Record::Fields(count) => {
                    let fields: Vec<Vec<u8>> = records.fields().map(<[u8]>::to_vec).collect();
                    assert_eq!(fields.len(), count);
                    out.push(Ok(fields));
                }
                refused => {
                    out.push(Err(refused));
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn records_match_the_csv_crate() {
        for (text, delimiter) in [
            (&b"a,b\n1,2\n"[..], b','),
            (
                b"\xef\xbb\xbfa,b\r\n\"x\r\ny\",\"say \"\"hi\"\"\"\r\n\r\n\r\n3,\n",
                b',',
            ),
            (b"a\r1\r\r2", b','),
            (b"\n\na,b,c\n,,\n1,\"\",3", b','),
            (b"a\tb\n1\t\"x\ty\"\n", b'\t'),
            (b"a,b\nno quote\"here,2\n", b','),
            (b"a,b\n\"open,2\n", b','),
            (b"", b','),
            (b"\n\r\n", b','),
            ("\u{e9}t\u{e9},b\n\u{2603},\"\u{1f600}\"\n".as_bytes(), b','),
        ] {
            let mut csv = csv::ReaderBuilder::new()
                .delimiter(delimiter)
                .has_headers(false)
                .flexible(true)
                .from_reader(text);
            let mut want = Vec::new();
            let mut record = csv::ByteRecord::new();
            while csv.read_byte_record(&mut record).unwrap() {
                want.push(Ok(record.iter().map(<[u8]>::to_vec).collect::<Vec<_>>()));
            }
            assert_eq!(
                all(text, delimiter, 8, 64),
                want,
                "{:?}",
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn a_record_is_refused_when_its_room_fills() {
        let wide = b",".repeat(2_000_000);
        assert_eq!(
            all(&wide, b',', 1_000, wide.len()),
            [Err(Record::TooManyFields)]
        );
        let mut records = Records::new(&wide[..], b',', 1_000, wide.len());
        records.read_record().unwrap();
        assert!(records.ends.len() == 1_001, "the field room never grows");
        // The limits are inclusive, with or without a final record end.
        for text in [&b"a,b,c\n"[..], b"a,b,c"] {
            assert_eq!(all(text, b',', 3, 64).len(), 1);
            assert_eq!(all(text, b',', 2, 64), [Err(Record::TooManyFields)]);
        }
        assert_eq!(all(b"a,\"bcd\"\n1,2\n", b',', 2, 4).len(), 2);
        assert_eq!(
            all(b"a,\"bcde\"\n1,2\n", b',', 2, 4),
            [Err(Record::TooLong)]
        );
        let long = b"x".repeat(300_000);
        assert_eq!(all(&long, b',', 2, 300_000), [Ok(vec![long.clone()])]);
        let mut records = Records::new(&long[..], b',', 2, 299_999);
        assert_eq!(records.read_record().unwrap(), Some(Record::TooLong));
        assert!(
            records.output.len() == 300_000,
            "the text room stops at its limit"
        );
        let ended = [&long[..], b"\n"].concat();
        assert_eq!(all(&ended, b',', 2, 299_999), [Err(Record::TooLong)]);
    }

    #[test]
    fn a_record_start_counts_the_blank_lines_before_it() {
        let text = b"a\n\n\r\nb\n";
        let mut records = Records::new(&text[..], b',', 4, 64);
        records.read_record().unwrap();
        assert_eq!(records.start(), 0);
        records.read_record().unwrap();
        assert_eq!(records.start(), 2);
    }
}
