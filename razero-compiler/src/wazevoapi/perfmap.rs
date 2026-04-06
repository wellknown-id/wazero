//! Perf-map support for optional profiling integration.

use std::fmt::Write as _;
use std::io::{self, Write};

pub const PERF_MAP_ENABLED: bool = option_env!("CARGO_FEATURE_PERFMAP").is_some();

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    index: usize,
    offset: i64,
    size: u64,
    name: String,
}

#[derive(Debug)]
pub struct Perfmap<W: Write> {
    entries: Vec<Entry>,
    writer: W,
}

impl<W: Write> Perfmap<W> {
    pub fn new(writer: W) -> Self {
        Self {
            entries: Vec::new(),
            writer,
        }
    }

    pub fn add_module_entry(
        &mut self,
        index: usize,
        offset: i64,
        size: u64,
        name: impl Into<String>,
    ) {
        self.entries.push(Entry {
            index,
            offset,
            size,
            name: name.into(),
        });
    }

    pub fn flush(&mut self, addr: usize, function_offsets: &[usize]) -> io::Result<()> {
        let mut line = String::new();
        for entry in &self.entries {
            line.clear();
            let absolute = addr
                .wrapping_add_signed(entry.offset as isize)
                .wrapping_add(function_offsets[entry.index]);
            let _ = write!(&mut line, "{absolute:x} {:x} {}\n", entry.size, entry.name);
            self.writer.write_all(line.as_bytes())?;
        }
        self.writer.flush()?;
        self.entries.clear();
        Ok(())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn add_entry(&mut self, addr: usize, size: u64, name: impl AsRef<str>) -> io::Result<()> {
        writeln!(self.writer, "{addr:x} {size:x} {}", name.as_ref())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::Perfmap;

    #[test]
    fn perfmap_flushes_module_entries() {
        let writer = Cursor::new(Vec::<u8>::new());
        let mut perfmap = Perfmap::new(writer);
        perfmap.add_module_entry(1, 0x20, 0x30, "f");
        perfmap.flush(0x1000, &[0x0, 0x10]).unwrap();
        let output = String::from_utf8(perfmap.into_inner().into_inner()).unwrap();
        assert_eq!(output, "1030 30 f\n");
    }

    #[test]
    fn perfmap_add_entry_writes_directly() {
        let writer = Cursor::new(Vec::<u8>::new());
        let mut perfmap = Perfmap::new(writer);
        perfmap.add_entry(0x2000, 0x40, "g").unwrap();
        let output = String::from_utf8(perfmap.into_inner().into_inner()).unwrap();
        assert_eq!(output, "2000 40 g\n");
    }
}
