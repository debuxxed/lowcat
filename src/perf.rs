use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static ENABLED: OnceLock<bool> = OnceLock::new();
static MIN_DURATION: OnceLock<Duration> = OnceLock::new();
static SAMPLES: OnceLock<Mutex<HashMap<&'static str, SampleStats>>> = OnceLock::new();

struct SampleStats {
    started: Instant,
    last: Instant,
    count: u64,
    total_gap: Duration,
    max_gap: Duration,
}

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

pub fn sample(label: &'static str) {
    if !enabled() {
        return;
    }

    let now = Instant::now();
    let mut samples = SAMPLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("perf samples mutex poisoned");
    let stats = samples.entry(label).or_insert_with(|| SampleStats {
        started: now,
        last: now,
        count: 0,
        total_gap: Duration::ZERO,
        max_gap: Duration::ZERO,
    });

    if stats.count > 0 {
        let gap = now.duration_since(stats.last);
        stats.total_gap += gap;
        stats.max_gap = stats.max_gap.max(gap);
    }
    stats.last = now;
    stats.count += 1;

    let elapsed = now.duration_since(stats.started);
    if elapsed < Duration::from_secs(1) {
        return;
    }

    let gaps = stats.count.saturating_sub(1);
    let avg_gap_ms = if gaps > 0 {
        stats.total_gap.as_secs_f64() * 1000. / gaps as f64
    } else {
        0.
    };
    let rate = stats.count as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[lowcat:perf] {label} rate={rate:.1}/s count={} avg_gap={avg_gap_ms:.2}ms max_gap={:.2}ms",
        stats.count,
        stats.max_gap.as_secs_f64() * 1000.
    );

    *stats = SampleStats {
        started: now,
        last: now,
        count: 0,
        total_gap: Duration::ZERO,
        max_gap: Duration::ZERO,
    };
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
