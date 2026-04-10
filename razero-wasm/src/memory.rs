#![doc = "Runtime-side linear memory instances."]

use std::ops::{Deref, DerefMut};

use razero_secmem::{GuardPageAllocator, GuardedAllocation, SecMemError};

use crate::memory_definition::MemoryDefinition;
use crate::module::Memory;

pub const MEMORY_PAGE_SIZE: u32 = 65_536;
pub const MEMORY_LIMIT_PAGES: u32 = 65_536;
pub const MEMORY_PAGE_SIZE_IN_BITS: u32 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBytes {
    Plain(Vec<u8>),
    Guarded {
        allocation: GuardedAllocation,
        len: usize,
    },
}

impl Default for MemoryBytes {
    fn default() -> Self {
        Self::Plain(Vec::new())
    }
}

impl From<Vec<u8>> for MemoryBytes {
    fn from(value: Vec<u8>) -> Self {
        Self::Plain(value)
    }
}

impl Deref for MemoryBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Plain(bytes) => bytes,
            Self::Guarded { allocation, len } => &allocation.as_slice()[..*len],
        }
    }
}

impl DerefMut for MemoryBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Plain(bytes) => bytes,
            Self::Guarded { allocation, len } => &mut allocation.as_mut_slice()[..*len],
        }
    }
}

impl MemoryBytes {
    pub fn len(&self) -> usize {
        match self {
            Self::Plain(bytes) => bytes.len(),
            Self::Guarded { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Plain(bytes) => bytes.is_empty(),
            Self::Guarded { len, .. } => *len == 0,
        }
    }

    pub fn resize(&mut self, new_len: usize, value: u8) {
        match self {
            Self::Plain(bytes) => bytes.resize(new_len, value),
            Self::Guarded { allocation, len } => {
                let slice = allocation.as_mut_slice();
                let old_len = *len;
                if new_len > slice.len() {
                    panic!("guarded resize beyond reserved length");
                }
                if new_len > old_len {
                    slice[old_len..new_len].fill(value);
                } else if new_len < old_len {
                    slice[new_len..old_len].fill(0);
                }
                *len = new_len;
            }
        }
    }

    pub fn reserved_len(&self) -> usize {
        match self {
            Self::Plain(bytes) => bytes.len(),
            Self::Guarded { allocation, .. } => allocation.len(),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.deref().to_vec()
    }
}

impl MemoryBytes {
    pub fn guarded(allocation: GuardedAllocation, len: usize) -> Self {
        let reserved_len = allocation.len();
        assert!(
            len <= reserved_len,
            "guarded visible length exceeds reservation"
        );
        Self::Guarded { allocation, len }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryInstance {
    pub bytes: MemoryBytes,
    pub min: u32,
    pub cap: u32,
    pub max: u32,
    pub shared: bool,
    pub definition: Option<MemoryDefinition>,
}

impl Default for MemoryInstance {
    fn default() -> Self {
        Self {
            bytes: Vec::new().into(),
            min: 0,
            cap: 0,
            max: 0,
            shared: false,
            definition: None,
        }
    }
}

impl MemoryInstance {
    pub fn new(memory: &Memory) -> Self {
        let min_bytes = memory_pages_to_bytes_num(memory.min) as usize;
        let cap = memory.cap.max(memory.min);
        let cap_bytes = memory_pages_to_bytes_num(cap) as usize;
        let mut bytes = Vec::with_capacity(cap_bytes);
        bytes.resize(min_bytes, 0);

        Self {
            bytes: bytes.into(),
            min: memory.min,
            cap,
            max: memory.max,
            shared: memory.is_shared,
            definition: None,
        }
    }

    pub fn new_guarded(memory: &Memory) -> Result<Self, SecMemError> {
        let min_bytes = memory_pages_to_bytes_num(memory.min) as usize;
        let cap = memory.cap.max(memory.min);
        let cap_bytes = memory_pages_to_bytes_num(cap) as usize;
        let mut bytes = GuardPageAllocator.allocate_zeroed(cap_bytes)?;
        if cap_bytes > min_bytes {
            bytes.as_mut_slice()[min_bytes..cap_bytes].fill(0);
        }
        Ok(Self {
            bytes: MemoryBytes::guarded(bytes, min_bytes),
            min: memory.min,
            cap,
            max: memory.max,
            shared: memory.is_shared,
            definition: None,
        })
    }

    pub fn with_definition(mut self, definition: MemoryDefinition) -> Self {
        self.definition = Some(definition);
        self
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn size(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn pages(&self) -> u32 {
        memory_bytes_num_to_pages(self.bytes.len() as u64)
    }

    pub fn has_size(&self, offset: u32, byte_count: u64) -> bool {
        u64::from(offset) + byte_count <= self.bytes.len() as u64
    }

    pub fn read_byte(&self, offset: u32) -> Option<u8> {
        self.has_size(offset, 1)
            .then(|| self.bytes[offset as usize])
    }

    pub fn read_u16_le(&self, offset: u32) -> Option<u16> {
        self.get_range::<2>(offset)
            .map(|raw| u16::from_le_bytes(*raw))
    }

    pub fn read_u32_le(&self, offset: u32) -> Option<u32> {
        self.get_range::<4>(offset)
            .map(|raw| u32::from_le_bytes(*raw))
    }

    pub fn read_f32_le(&self, offset: u32) -> Option<f32> {
        self.read_u32_le(offset).map(f32::from_bits)
    }

    pub fn read_u64_le(&self, offset: u32) -> Option<u64> {
        self.get_range::<8>(offset)
            .map(|raw| u64::from_le_bytes(*raw))
    }

    pub fn read_f64_le(&self, offset: u32) -> Option<f64> {
        self.read_u64_le(offset).map(f64::from_bits)
    }

    pub fn read(&self, offset: u32, byte_count: u32) -> Option<&[u8]> {
        self.has_size(offset, u64::from(byte_count)).then(|| {
            let start = offset as usize;
            let end = start + byte_count as usize;
            &self.bytes[start..end]
        })
    }

    pub fn read_mut(&mut self, offset: u32, byte_count: u32) -> Option<&mut [u8]> {
        self.has_size(offset, u64::from(byte_count)).then(|| {
            let start = offset as usize;
            let end = start + byte_count as usize;
            &mut self.bytes[start..end]
        })
    }

    pub fn write_byte(&mut self, offset: u32, value: u8) -> bool {
        match self.bytes.get_mut(offset as usize) {
            Some(byte) => {
                *byte = value;
                true
            }
            None => false,
        }
    }

    pub fn write_u16_le(&mut self, offset: u32, value: u16) -> bool {
        self.write_bytes(offset, &value.to_le_bytes())
    }

    pub fn write_u32_le(&mut self, offset: u32, value: u32) -> bool {
        self.write_bytes(offset, &value.to_le_bytes())
    }

    pub fn write_f32_le(&mut self, offset: u32, value: f32) -> bool {
        self.write_u32_le(offset, value.to_bits())
    }

    pub fn write_u64_le(&mut self, offset: u32, value: u64) -> bool {
        self.write_bytes(offset, &value.to_le_bytes())
    }

    pub fn write_f64_le(&mut self, offset: u32, value: f64) -> bool {
        self.write_u64_le(offset, value.to_bits())
    }

    pub fn write(&mut self, offset: u32, values: &[u8]) -> bool {
        self.write_bytes(offset, values)
    }

    pub fn write_string(&mut self, offset: u32, value: &str) -> bool {
        self.write(offset, value.as_bytes())
    }

    pub fn grow(&mut self, delta: u32) -> Option<u32> {
        let current_pages = self.pages();
        if delta == 0 {
            return Some(current_pages);
        }

        let new_pages = current_pages.checked_add(delta)?;
        if new_pages > self.max {
            return None;
        }

        let new_len = memory_pages_to_bytes_num(new_pages) as usize;
        if new_pages > self.cap {
            self.bytes.resize(new_len, 0);
            self.cap = new_pages;
        } else {
            self.bytes.resize(new_len, 0);
        }

        Some(current_pages)
    }

    fn write_bytes(&mut self, offset: u32, values: &[u8]) -> bool {
        if !self.has_size(offset, values.len() as u64) {
            return false;
        }

        let start = offset as usize;
        let end = start + values.len();
        self.bytes[start..end].copy_from_slice(values);
        true
    }

    fn get_range<const N: usize>(&self, offset: u32) -> Option<&[u8; N]> {
        if !self.has_size(offset, N as u64) {
            return None;
        }

        let start = offset as usize;
        let end = start + N;
        self.bytes[start..end].try_into().ok()
    }
}

pub fn memory_pages_to_bytes_num(pages: u32) -> u64 {
    u64::from(pages) << MEMORY_PAGE_SIZE_IN_BITS
}

pub fn memory_bytes_num_to_pages(bytes_num: u64) -> u32 {
    (bytes_num >> MEMORY_PAGE_SIZE_IN_BITS) as u32
}

pub fn pages_to_unit_of_bytes(pages: u32) -> String {
    let kib = pages.saturating_mul(64);
    if kib < 1024 {
        return format!("{kib} Ki");
    }

    let mib = kib / 1024;
    if mib < 1024 {
        return format!("{mib} Mi");
    }

    let gib = mib / 1024;
    if gib < 1024 {
        return format!("{gib} Gi");
    }

    format!("{} Ti", gib / 1024)
}

#[cfg(test)]
mod tests {
    use super::{
        memory_bytes_num_to_pages, memory_pages_to_bytes_num, pages_to_unit_of_bytes,
        MemoryInstance, MEMORY_LIMIT_PAGES, MEMORY_PAGE_SIZE, MEMORY_PAGE_SIZE_IN_BITS,
    };
    use crate::module::Memory;

    #[test]
    fn memory_page_constants_match() {
        assert_eq!(MEMORY_PAGE_SIZE, 1_u32 << MEMORY_PAGE_SIZE_IN_BITS);
        assert_eq!(MEMORY_LIMIT_PAGES, 1_u32 << 16);
    }

    #[test]
    fn memory_page_conversions_match() {
        for pages in [0, 1, 5, 10] {
            assert_eq!(
                memory_pages_to_bytes_num(pages),
                u64::from(pages) * u64::from(MEMORY_PAGE_SIZE)
            );
        }

        for bytes in [0, MEMORY_PAGE_SIZE, MEMORY_PAGE_SIZE * 10] {
            assert_eq!(
                memory_bytes_num_to_pages(u64::from(bytes)),
                bytes / MEMORY_PAGE_SIZE
            );
        }
    }

    #[test]
    fn grow_respects_max_and_returns_previous_pages() {
        let mut memory = MemoryInstance::new(&Memory {
            min: 0,
            cap: 0,
            max: 10,
            is_max_encoded: true,
            is_shared: false,
        });

        assert_eq!(memory.grow(5), Some(0));
        assert_eq!(memory.pages(), 5);
        assert_eq!(memory.grow(0), Some(5));
        assert_eq!(memory.grow(4), Some(5));
        assert_eq!(memory.pages(), 9);
        assert_eq!(memory.grow(2), None);
        assert_eq!(memory.pages(), 9);
        assert_eq!(memory.grow(1), Some(9));
        assert_eq!(memory.pages(), 10);
    }

    #[test]
    fn has_size_rejects_overflow() {
        let memory = MemoryInstance {
            bytes: vec![0; MEMORY_PAGE_SIZE as usize].into(),
            ..MemoryInstance::default()
        };

        assert!(memory.has_size(0, 8));
        assert!(memory.has_size(memory.size() - 8, 8));
        assert!(!memory.has_size(100, u64::from(memory.size() - 99)));
        assert!(!memory.has_size(memory.size(), 1));
        assert!(!memory.has_size(u32::MAX - 1, 4));
        assert!(!memory.has_size(u32::MAX, 1));
    }

    #[test]
    fn reads_and_writes_are_little_endian_and_bounds_checked() {
        let mut memory = MemoryInstance {
            bytes: vec![0; 16].into(),
            ..MemoryInstance::default()
        };

        assert!(memory.write_byte(0, 0xaa));
        assert!(memory.write_u16_le(1, 0xbccd));
        assert!(memory.write_u32_le(4, 0x1122_3344));
        assert!(memory.write_u64_le(8, 0x0102_0304_0506_0708));

        assert_eq!(memory.read_byte(0), Some(0xaa));
        assert_eq!(memory.read_u16_le(1), Some(0xbccd));
        assert_eq!(memory.read_u32_le(4), Some(0x1122_3344));
        assert_eq!(memory.read_u64_le(8), Some(0x0102_0304_0506_0708));

        assert_eq!(memory.read_u16_le(15), None);
        assert!(!memory.write_u32_le(13, 1));
    }

    #[test]
    fn read_mut_and_write_string_share_memory() {
        let mut memory = MemoryInstance {
            bytes: vec![0; 8].into(),
            ..MemoryInstance::default()
        };

        assert!(memory.write_string(2, "rust"));
        assert_eq!(memory.read(2, 4), Some("rust".as_bytes()));

        let window = memory.read_mut(2, 4).expect("slice");
        window[3] = b'!';
        assert_eq!(memory.read(2, 4), Some("rus!".as_bytes()));

        assert!(!memory.write(([0; 0].len() as u32) + 8, b"x"));
    }

    #[test]
    fn pages_render_to_human_units() {
        assert_eq!(pages_to_unit_of_bytes(0), "0 Ki");
        assert_eq!(pages_to_unit_of_bytes(1), "64 Ki");
        assert_eq!(pages_to_unit_of_bytes(100), "6 Mi");
        assert_eq!(pages_to_unit_of_bytes(MEMORY_LIMIT_PAGES), "4 Gi");
        assert_eq!(pages_to_unit_of_bytes(u32::MAX), "3 Ti");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guarded_constructor_uses_guarded_backing() {
        let memory = Memory {
            min: 1,
            cap: 2,
            max: 2,
            is_max_encoded: true,
            is_shared: false,
        };
        let guarded = MemoryInstance::new_guarded(&memory).expect("guarded allocation");
        assert!(matches!(guarded.bytes, super::MemoryBytes::Guarded { .. }));
        assert_eq!(MEMORY_PAGE_SIZE as usize, guarded.len());
        assert_eq!(2, guarded.cap);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guarded_memory_grow_preserves_backing_and_zero_fills_new_pages() {
        let memory = Memory {
            min: 1,
            cap: 3,
            max: 3,
            is_max_encoded: true,
            is_shared: false,
        };
        let mut guarded = MemoryInstance::new_guarded(&memory).expect("guarded allocation");
        let reserved_len = guarded.bytes.reserved_len();

        assert!(matches!(guarded.bytes, super::MemoryBytes::Guarded { .. }));
        assert!(guarded.write_u32_le(MEMORY_PAGE_SIZE - 4, 0x1122_3344));
        assert_eq!(Some(0x1122_3344), guarded.read_u32_le(MEMORY_PAGE_SIZE - 4));
        assert_eq!(Some(1), guarded.grow(1));
        assert!(matches!(guarded.bytes, super::MemoryBytes::Guarded { .. }));
        assert_eq!(reserved_len, guarded.bytes.reserved_len());
        assert_eq!(2, guarded.pages());
        assert_eq!(Some(0), guarded.read_byte(MEMORY_PAGE_SIZE));
        assert!(guarded.write_u32_le(MEMORY_PAGE_SIZE, 0x5566_7788));
        assert_eq!(Some(0x5566_7788), guarded.read_u32_le(MEMORY_PAGE_SIZE));
    }
}
