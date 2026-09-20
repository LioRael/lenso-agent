pub const DEFAULT_RETRY_BUDGET: u8 = 3;

pub fn attempts_per_job() -> u8 {
    DEFAULT_RETRY_BUDGET + 1
}
