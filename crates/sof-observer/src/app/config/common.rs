use std::time::Duration;

pub(crate) use crate::runtime_env::read_env_var;

pub fn read_bool_env(name: &str, default: bool) -> bool {
    read_env_var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

pub fn duration_to_ms_u64(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    if millis > u128::from(u64::MAX) {
        u64::MAX
    } else {
        millis as u64
    }
}
