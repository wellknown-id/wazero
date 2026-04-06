//! Register-set utilities.

use std::fmt;

use super::reg::{RealReg, VRegId};
use super::regalloc::RegisterInfo;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct RegSet(u64);

impl RegSet {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn from_regs(regs: &[RealReg]) -> Self {
        let mut ret = Self::new();
        for &reg in regs {
            ret = ret.add(reg);
        }
        ret
    }

    pub const fn has(self, r: RealReg) -> bool {
        r.0 < 64 && (self.0 & (1u64 << r.0)) != 0
    }

    pub const fn add(self, r: RealReg) -> Self {
        if r.0 >= 64 {
            self
        } else {
            Self(self.0 | (1u64 << r.0))
        }
    }

    pub fn for_each(self, mut f: impl FnMut(RealReg)) {
        for index in 0..64 {
            if (self.0 & (1u64 << index)) != 0 {
                f(RealReg(index as u8));
            }
        }
    }

    pub fn format(self, info: &RegisterInfo) -> String {
        let mut names = Vec::new();
        self.for_each(|reg| names.push((info.real_reg_name)(reg)));
        names.join(", ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RegInUseSet([Option<VRegId>; 64]);

impl RegInUseSet {
    pub(crate) const fn new() -> Self {
        Self([None; 64])
    }

    pub(crate) fn reset(&mut self) {
        self.0 = [None; 64];
    }

    pub(crate) const fn has(&self, r: RealReg) -> bool {
        r.0 < 64 && self.0[r.0 as usize].is_some()
    }

    pub(crate) const fn get(&self, r: RealReg) -> Option<VRegId> {
        if r.0 < 64 {
            self.0[r.0 as usize]
        } else {
            None
        }
    }

    pub(crate) fn remove(&mut self, r: RealReg) {
        if r.0 < 64 {
            self.0[r.0 as usize] = None;
        }
    }

    pub(crate) fn add(&mut self, r: RealReg, vreg: VRegId) {
        if r.0 < 64 {
            self.0[r.0 as usize] = Some(vreg);
        }
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (RealReg, VRegId)> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.map(|vreg| (RealReg(index as u8), vreg)))
    }
}

impl Default for RegInUseSet {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RegSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::reg::RealReg;
    use super::{RegInUseSet, RegSet};

    #[test]
    fn reg_set_tracks_membership() {
        let set = RegSet::from_regs(&[RealReg(1), RealReg(3), RealReg(63)]);
        assert!(set.has(RealReg(1)));
        assert!(set.has(RealReg(3)));
        assert!(set.has(RealReg(63)));
        assert!(!set.has(RealReg(2)));
        assert!(!set.add(RealReg(64)).has(RealReg(64)));
    }

    #[test]
    fn reg_in_use_set_tracks_occupants() {
        let mut in_use = RegInUseSet::new();
        in_use.add(RealReg(4), 12);
        assert!(in_use.has(RealReg(4)));
        assert_eq!(in_use.get(RealReg(4)), Some(12));
        in_use.remove(RealReg(4));
        assert_eq!(in_use.get(RealReg(4)), None);
    }
}
