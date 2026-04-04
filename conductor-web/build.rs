use std::process::Command;

fn main() {
    // Re-run if HEAD changes (new commit)
    println!("cargo:rerun-if-changed=.git/HEAD");

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    let build_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            // Format as RFC 3339 UTC: YYYY-MM-DDTHH:MM:SSZ
            let s = secs % 60;
            let m = (secs / 60) % 60;
            let h = (secs / 3600) % 24;
            let days = secs / 86400;
            // Days since 1970-01-01
            let (y, mo, day) = days_to_ymd(days);
            format!("{y:04}-{mo:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
        })
        .unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_TIMESTAMP={build_timestamp}");
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Gregorian calendar calculation from days since 1970-01-01
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
