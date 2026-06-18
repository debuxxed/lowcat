use std::sync::OnceLock;
use std::time::{Duration, Instant};

static ENABLED: OnceLock<bool> = OnceLock::new();
static MIN_DURATION: OnceLock<Duration> = OnceLock::new();

pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

pub fn finish(label: &str, start: Option<Instant>, details: impl FnOnce() -> String) {
    let Some(start) = start else {
        return;
    };

    let elapsed = start.elapsed();
    if elapsed < min_duration() {
        return;
    }

    eprintln!(
        "[lowcat:perf] {label} {:.2}ms {}",
        elapsed.as_secs_f64() * 1000.,
        details()
    );
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("LOWCAT_PROFILE")
            .map(|value| value != "0")
            .unwrap_or(false)
    })
}

fn min_duration() -> Duration {
    *MIN_DURATION.get_or_init(|| {
        let ms = std::env::var("LOWCAT_PROFILE_MIN_MS")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(1.);
        Duration::from_secs_f64((ms / 1000.).max(0.))
    })
}
