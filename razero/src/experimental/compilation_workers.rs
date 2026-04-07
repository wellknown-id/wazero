use crate::ctx_keys::Context;

pub fn with_compilation_workers(ctx: &Context, workers: usize) -> Context {
    let mut cloned = ctx.clone();
    cloned.compilation_workers = workers;
    cloned
}

pub fn get_compilation_workers(ctx: &Context) -> usize {
    ctx.compilation_workers.max(1)
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
    fn preserves_positive_worker_count() {
        let ctx = with_compilation_workers(&Context::default(), 4);
        assert_eq!(4, get_compilation_workers(&ctx));
    }

    #[test]
    fn getter_clamps_zero_worker_count_to_one() {
        let ctx = with_compilation_workers(&Context::default(), 0);
        assert_eq!(0, ctx.compilation_workers);
        assert_eq!(1, get_compilation_workers(&ctx));
    }
}
