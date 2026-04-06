#![doc = "Memory type and section decoding."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero::CoreFeatures;
use razero_wasm::module::{Memory, MEMORY_LIMIT_PAGES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySizer {
    memory_limit_pages: u32,
    memory_capacity_from_max: bool,
}

impl Default for MemorySizer {
    fn default() -> Self {
        Self::new(MEMORY_LIMIT_PAGES, false)
    }
}

impl MemorySizer {
    pub const fn new(memory_limit_pages: u32, memory_capacity_from_max: bool) -> Self {
        Self {
            memory_limit_pages,
            memory_capacity_from_max,
        }
    }

    pub const fn memory_limit_pages(self) -> u32 {
        self.memory_limit_pages
    }

    pub fn size(self, min_pages: u32, max_pages: Option<u32>) -> (u32, u32, u32) {
        if let Some(max_pages) = max_pages {
            if self.memory_capacity_from_max {
                return (min_pages, max_pages, max_pages);
            }
            if max_pages > MEMORY_LIMIT_PAGES {
                return (min_pages, min_pages, max_pages);
            }
            if max_pages > self.memory_limit_pages {
                return (min_pages, min_pages, self.memory_limit_pages);
            }
            return (min_pages, min_pages, max_pages);
        }

        if self.memory_capacity_from_max {
            (min_pages, self.memory_limit_pages, self.memory_limit_pages)
        } else {
            (min_pages, min_pages, self.memory_limit_pages)
        }
    }
}

pub(crate) fn decode_limits_type(
    decoder: &mut Decoder<'_>,
) -> DecodeResult<(u32, Option<u32>, bool)> {
    let flag = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read leading byte: {}", err.message)))?;

    let (min, max) = match flag {
        0x00 | 0x02 => {
            let min = decoder
                .read_var_u32("read min of limit")
                .map_err(|err| DecodeError::new(err.message))?;
            (min, None)
        }
        0x01 | 0x03 => {
            let min = decoder
                .read_var_u32("read min of limit")
                .map_err(|err| DecodeError::new(err.message))?;
            let max = decoder
                .read_var_u32("read max of limit")
                .map_err(|err| DecodeError::new(err.message))?;
            (min, Some(max))
        }
        _ => {
            return Err(DecodeError::new(format!(
                "invalid byte for limits: {flag:#x} not in (0x00, 0x01, 0x02, 0x03)"
            )))
        }
    };

    Ok((min, max, flag == 0x02 || flag == 0x03))
}

pub fn decode_memory(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    memory_sizer: MemorySizer,
) -> DecodeResult<Memory> {
    let (min, max_pages, shared) = decode_limits_type(decoder)?;

    if shared && !enabled_features.contains(CoreFeatures::THREADS) {
        return Err(DecodeError::new(
            "shared memory requested but threads feature not enabled",
        ));
    }
    if shared && max_pages.is_none() {
        return Err(DecodeError::new(
            "shared memory requires a maximum size to be specified",
        ));
    }

    let (min, cap, max) = memory_sizer.size(min, max_pages);
    let memory = Memory {
        min,
        cap,
        max,
        is_max_encoded: max_pages.is_some(),
        is_shared: shared,
    };
    memory
        .validate(memory_sizer.memory_limit_pages())
        .map_err(|err| DecodeError::new(err.to_string()))?;
    Ok(memory)
}

pub fn decode_memory_section(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    memory_sizer: MemorySizer,
) -> DecodeResult<Option<Memory>> {
    let count = decoder
        .read_var_u32("error reading size")
        .map_err(|err| DecodeError::new(err.message))?;

    if count > 1 {
        return Err(DecodeError::new(format!(
            "at most one memory allowed in module, but read {count}"
        )));
    }
    if count == 0 {
        return Ok(None);
    }

    decode_memory(decoder, enabled_features, memory_sizer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{decode_limits_type, decode_memory, decode_memory_section, MemorySizer};
    use crate::decoder::Decoder;
    use razero::CoreFeatures;
    use razero_wasm::module::{Memory, MEMORY_LIMIT_PAGES};

    #[test]
    fn memory_sizer_matches_go_rules() {
        let sizer = MemorySizer::new(200, false);
        assert_eq!((10, 10, 200), sizer.size(10, None));
        assert_eq!((0, 0, 10), sizer.size(0, Some(10)));

        let sizer = MemorySizer::new(200, true);
        assert_eq!((10, 200, 200), sizer.size(10, None));
        assert_eq!((0, 10, 10), sizer.size(0, Some(10)));
    }

    #[test]
    fn decodes_memory_and_limits() {
        let mut decoder = Decoder::new(&[0x01, 0x02, 0x03]);
        let actual =
            decode_memory(&mut decoder, CoreFeatures::all(), MemorySizer::default()).unwrap();

        assert_eq!(
            Memory {
                min: 2,
                cap: 2,
                max: 3,
                is_max_encoded: true,
                is_shared: false,
            },
            actual
        );
        assert_eq!(0, decoder.remaining());
    }

    #[test]
    fn rejects_shared_memory_without_threads() {
        let mut decoder = Decoder::new(&[0x03, 0x00, 0x01]);
        let err =
            decode_memory(&mut decoder, CoreFeatures::empty(), MemorySizer::default()).unwrap_err();

        assert_eq!(
            "shared memory requested but threads feature not enabled",
            err.message
        );
    }

    #[test]
    fn rejects_multiple_memories_in_section() {
        let mut decoder = Decoder::new(&[0x02, 0x00, 0x01, 0x01, 0x02]);
        let err = decode_memory_section(&mut decoder, CoreFeatures::all(), MemorySizer::default())
            .unwrap_err();

        assert_eq!(
            "at most one memory allowed in module, but read 2",
            err.message
        );
    }

    #[test]
    fn decodes_limits_flags() {
        let mut decoder = Decoder::new(&[0x03, 0x00, 0x80, 0x80, 0x04]);
        let actual = decode_limits_type(&mut decoder).unwrap();

        assert_eq!((0, Some(MEMORY_LIMIT_PAGES), true), actual);
    }
}
