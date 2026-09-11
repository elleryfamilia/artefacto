//! RFC 3339 timestamps in UTC to the second, without a date crate. The
//! consumers are a person reading a log, a client echoing a string back, and
//! the index's relative ages, none of which needs more than this.

/// Now, as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_rfc3339() -> String {
    rfc3339(now_secs())
}

/// Seconds since the Unix epoch, saturating at zero for a clock set before it.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// The seconds since the epoch of a `YYYY-MM-DDTHH:MM:SSZ` string, or `None`
/// for anything else. Only the form this crate writes is read back; an
/// offset other than `Z` or a fractional second is not one of its strings.
pub fn parse_rfc3339(ts: &str) -> Option<u64> {
    let bytes = ts.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u64> { ts.get(from..to)?.parse::<u64>().ok() };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = days_from_civil(y as i64, m as u32, d as u32);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Howard Hinnant's days-to-civil algorithm, public domain.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The inverse: days since the epoch of a civil date. Same source.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        // A leap day, where naive implementations go wrong.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for days in [0, 1, 58, 59, 19_000, 19_782, 20_000, 20_700] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
    }

    #[test]
    fn a_timestamp_round_trips_to_the_second() {
        for secs in [0u64, 59, 86_399, 86_400, 1_700_000_000, 1_789_000_000] {
            let text = rfc3339(secs);
            assert_eq!(parse_rfc3339(&text), Some(secs), "{text}");
        }
        assert_eq!(rfc3339(1_789_000_000), "2026-09-10T00:26:40Z");
    }

    #[test]
    fn anything_but_this_crates_own_form_is_refused() {
        for bad in [
            "",
            "2026-09-10",
            "2026-09-10T05:46:40",
            "2026-09-10T05:46:40+00:00",
            "2026-09-10T05:46:40.000Z",
            "2026-13-10T05:46:40Z",
            "2026-09-10T25:46:40Z",
            "yesterday",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?}");
        }
    }
}
