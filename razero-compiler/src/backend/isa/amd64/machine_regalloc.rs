use crate::backend::regalloc::RegisterInfo;

use super::abi::amd64_register_info;

pub fn regalloc_register_info() -> RegisterInfo {
    amd64_register_info()
}

#[cfg(test)]
mod tests {
    use super::regalloc_register_info;

    #[test]
    fn register_info_smoke_test() {
        let info = regalloc_register_info();
        assert_eq!(
            (info.real_reg_name)(crate::backend::regalloc::RealReg(1)),
            "rax"
        );
    }
}
