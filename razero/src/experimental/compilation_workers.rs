use crate::ctx_keys::Context;

pub fn with_compilation_workers(ctx: &Context, workers: isize) -> Context {
    let mut cloned = ctx.clone();
    cloned.compilation_workers = Some(workers);
    cloned
}

pub fn get_compilation_workers(ctx: &Context) -> usize {
    ctx.compilation_workers.unwrap_or(1).max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::{get_compilation_workers, with_compilation_workers};
    use crate::ctx_keys::Context;

    #[test]
    fn defaults_to_one() {
        assert_eq!(1, get_compilation_workers(&Context::default()));
    }

    #[test]
    fn preserves_single_worker_count() {
        let ctx = with_compilation_workers(&Context::default(), 1);
        assert_eq!(1, get_compilation_workers(&ctx));
    }

    #[test]
    fn getter_clamps_zero_worker_count_to_one() {
        let ctx = with_compilation_workers(&Context::default(), 0);
        assert_eq!(1, get_compilation_workers(&ctx));
    }

    #[test]
    fn getter_clamps_negative_worker_count_to_one() {
        let ctx = with_compilation_workers(&Context::default(), -7);
        assert_eq!(1, get_compilation_workers(&ctx));
    }

    #[test]
    fn preserves_positive_worker_count() {
        let ctx = with_compilation_workers(&Context::default(), 4);
        assert_eq!(4, get_compilation_workers(&ctx));
    }
}
