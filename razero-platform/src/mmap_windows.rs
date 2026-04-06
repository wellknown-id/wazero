use crate::mmap::{CodeSegment, MmapError};

const UNSUPPORTED: &str = "executable mmap is only implemented on Linux targets";

pub(crate) fn map_code_segment_impl(len: usize) -> Result<CodeSegment, MmapError> {
    if len == 0 {
        return Err(MmapError::ZeroLength);
    }

    Err(MmapError::Unsupported(UNSUPPORTED))
}

pub(crate) fn protect_code_segment_impl(_segment: &mut CodeSegment) -> Result<(), MmapError> {
    Err(MmapError::Unsupported(UNSUPPORTED))
}

pub(crate) fn unmap_code_segment_impl(_segment: &mut CodeSegment) -> Result<(), MmapError> {
    Err(MmapError::Unsupported(UNSUPPORTED))
}
