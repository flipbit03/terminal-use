use std::path::PathBuf;
use std::time::SystemTime;

/// Generate a timestamped PNG path in the system temp directory.
///
/// Format: `tu-screenshot-{session}-{YYYYMMDD}T{HHMMSS}.{tenths}.png`
///
/// Lexicographically sortable, includes session name and subsecond
/// precision to avoid collisions with concurrent sessions.
pub fn auto_png_path(session_name: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let secs = now.as_secs();
    let tenths = (now.subsec_millis() / 100) as u64;

    // Convert to broken-down time manually (UTC) to avoid adding chrono dep.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 → year/month/day.
    let (year, month, day) = {
        let mut y = 1970i64;
        let mut remaining = days as i64;
        loop {
            let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                366
            } else {
                365
            };
            if remaining < days_in_year {
                break;
            }
            remaining -= days_in_year;
            y += 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let month_days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0usize;
        while m < 12 && remaining >= month_days[m] {
            remaining -= month_days[m];
            m += 1;
        }
        (y, m + 1, remaining + 1)
    };

    let filename = format!(
        "tu-screenshot-{}-{:04}{:02}{:02}T{:02}{:02}{:02}.{}.png",
        session_name, year, month, day, hours, minutes, seconds, tenths
    );
    std::env::temp_dir().join(filename)
}

#[cfg(test)]
mod tests {
    use super::auto_png_path;

    #[test]
    fn auto_png_path_format() {
        let path = auto_png_path("myapp");
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.starts_with("tu-screenshot-myapp-"));
        assert!(filename.ends_with(".png"));
        // YYYYMMDD'T'HHMMSS.D.png — verify the 'T' separator is present
        assert!(filename.contains('T'));
    }

    #[test]
    fn auto_png_path_in_temp_dir() {
        let path = auto_png_path("test");
        assert_eq!(path.parent().unwrap(), std::env::temp_dir());
    }

    #[test]
    fn auto_png_path_embeds_session_name() {
        let path = auto_png_path("my-session");
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.contains("my-session"));
    }
}
