use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MS: i64 = 1_000_000;
pub const FAKE_EPOCH_NANOS: i64 = 1_640_995_200_000 * MS;

pub fn new_fake_walltime() -> impl Fn() -> (i64, i32) {
    let nanos = AtomicI64::new(FAKE_EPOCH_NANOS);
    move || {
        let walltime = nanos.fetch_add(MS, Ordering::SeqCst);
        (walltime / 1_000_000_000, (walltime % 1_000_000_000) as i32)
    }
}

pub fn new_fake_nanotime() -> impl Fn() -> i64 {
    let nanos = AtomicI64::new(MS);
    move || nanos.fetch_add(MS, Ordering::SeqCst)
}

pub fn walltime() -> (i64, i32) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    (now.as_secs() as i64, now.subsec_nanos() as i32)
}

pub fn nanotime() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

pub fn nanosleep(ns: i64) {
    if ns > 0 {
        std::thread::sleep(Duration::from_nanos(ns as u64));
    }
}

#[cfg(test)]
mod tests {
    use super::{new_fake_nanotime, new_fake_walltime, FAKE_EPOCH_NANOS};

    #[test]
    fn fake_walltime_starts_at_fixed_epoch_and_ticks_by_millisecond() {
        let walltime = new_fake_walltime();
        assert_eq!(
            (
                FAKE_EPOCH_NANOS / 1_000_000_000,
                (FAKE_EPOCH_NANOS % 1_000_000_000) as i32
            ),
            walltime()
        );
        assert_eq!(
            (
                (FAKE_EPOCH_NANOS + 1_000_000) / 1_000_000_000,
                ((FAKE_EPOCH_NANOS + 1_000_000) % 1_000_000_000) as i32,
            ),
            walltime()
        );
    }

    #[test]
    fn fake_nanotime_ticks_by_millisecond() {
        let nanotime = new_fake_nanotime();
        assert_eq!(1_000_000, nanotime());
        assert_eq!(2_000_000, nanotime());
    }
}
