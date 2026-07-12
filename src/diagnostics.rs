pub fn debug(scope: &str, details: impl FnOnce() -> String) {
    if enabled() {
        eprintln!("[lowcat:{scope}] {}", details());
    }
}

fn enabled() -> bool {
    std::env::var("LOWCAT_DEBUG")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}
