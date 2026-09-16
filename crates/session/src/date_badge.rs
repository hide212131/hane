//! Date badges for work-folder sidebar file names (issue #174).
//!
//! Finds a calendar date embedded in a Markdown file name (`YYYY-MM-DD` or
//! `YYYYMMDD`, anywhere in the name) without ever touching the real file
//! name, path, or work-folder sort key: those stay exactly what the
//! filesystem reports. What the sidebar renders is a display-only
//! decomposition of the name into the text before the date, the date
//! itself, and the text after, plus the short label the badge shows for
//! that date relative to "today".
//!
//! Everything here except [`local_today`] is pure and takes "today" as an
//! explicit argument, so the calendar rules stay unit-testable without
//! touching the system clock; `local_today` is the one boundary a caller
//! calls once and threads through.

use std::ops::Range;

/// A calendar date, no timezone attached: enough to validate the digits
/// found in a file name and to compare against "today".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalendarDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl CalendarDate {
    /// `None` when `year`/`month`/`day` is not a real proleptic-Gregorian
    /// calendar date (bad month, or a day past the end of its month —
    /// including a non-leap February 29th).
    #[must_use]
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        is_valid_calendar_date(year, month, day).then_some(Self { year, month, day })
    }

    /// Sunday = 0 .. Saturday = 6, per the proleptic Gregorian calendar.
    #[must_use]
    pub fn weekday_index(self) -> usize {
        let days = days_from_civil(self.year, self.month, self.day);
        usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0)
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_valid_calendar_date(year: i32, month: u32, day: u32) -> bool {
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

/// Days since the Unix epoch (1970-01-01) for a proleptic Gregorian date.
/// Howard Hinnant's `days_from_civil`, the standard integer-only
/// civil-calendar / day-count conversion (see
/// <http://howardhinnant.github.io/date_algorithms.html>).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let month_index = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`]: the proleptic Gregorian date `days`
/// after the Unix epoch.
fn civil_from_days(days: i64) -> CalendarDate {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    CalendarDate {
        year: i32::try_from(year).unwrap_or(0),
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

/// A date found in a file name, with the byte range it occupies so the
/// caller can split the original name around it without re-deriving the
/// span from the formatted date (which would not round-trip for a
/// `YYYYMMDD` name, since it drops the separators).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DateMatch {
    date: CalendarDate,
    start: usize,
    end: usize,
}

fn digit_at(bytes: &[u8], index: usize) -> bool {
    index < bytes.len() && bytes[index].is_ascii_digit()
}

fn parse_digits(bytes: &[u8], range: Range<usize>) -> Option<i32> {
    std::str::from_utf8(&bytes[range]).ok()?.parse().ok()
}

/// Never part of a longer run of digits: neither the byte just before
/// `start` nor the byte just at `end` may itself be a digit, so `20260917`
/// inside `120260917` (a nine-digit run) is correctly not a date.
fn has_digit_run_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    !digit_at(bytes, start.wrapping_sub(1)) && !digit_at(bytes, end)
}

/// Matches `YYYYMMDD` starting at `start`.
fn match_compact(bytes: &[u8], start: usize) -> Option<DateMatch> {
    let end = start.checked_add(8)?;
    let segment = bytes.get(start..end)?;
    if !segment.iter().all(u8::is_ascii_digit) || !has_digit_run_boundary(bytes, start, end) {
        return None;
    }
    let year = parse_digits(bytes, start..start + 4)?;
    let month = parse_digits(bytes, start + 4..start + 6)?;
    let day = parse_digits(bytes, start + 6..start + 8)?;
    let date = CalendarDate::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)?;
    Some(DateMatch { date, start, end })
}

/// Matches `YYYY-MM-DD` starting at `start`.
fn match_hyphenated(bytes: &[u8], start: usize) -> Option<DateMatch> {
    let end = start.checked_add(10)?;
    let segment = bytes.get(start..end)?;
    let digits_ok = segment[0..4].iter().all(u8::is_ascii_digit)
        && segment[5..7].iter().all(u8::is_ascii_digit)
        && segment[8..10].iter().all(u8::is_ascii_digit);
    if !digits_ok
        || segment[4] != b'-'
        || segment[7] != b'-'
        || !has_digit_run_boundary(bytes, start, end)
    {
        return None;
    }
    let year = parse_digits(bytes, start..start + 4)?;
    let month = parse_digits(bytes, start + 5..start + 7)?;
    let day = parse_digits(bytes, start + 8..start + 10)?;
    let date = CalendarDate::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)?;
    Some(DateMatch { date, start, end })
}

/// The first valid date found in `file_name`, trying every start position
/// left to right so a match anywhere — start, middle, or end — is found,
/// while a digit run that only partly looks like a date (too many digits
/// around it, or a calendar-invalid value such as day 30 of February) is
/// skipped rather than reported.
fn find_date(file_name: &str) -> Option<DateMatch> {
    let bytes = file_name.as_bytes();
    (0..bytes.len())
        .find_map(|start| match_hyphenated(bytes, start).or_else(|| match_compact(bytes, start)))
}

/// The sidebar's display-only breakdown of a file name around an embedded
/// date. `before` and `after` are trimmed of a directly adjacent run of
/// `_`, `-`, or ` ` — separators that only existed to set the date off from
/// the rest of the name and would otherwise look like a stray leftover once
/// the date renders as its own badge — but are otherwise exactly the
/// original file name's text, so nothing about the real file name changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileNameDateBadge {
    pub before: String,
    pub date: CalendarDate,
    pub after: String,
}

const BADGE_SEPARATORS: [char; 3] = ['_', '-', ' '];

/// `None` when `file_name` has no valid embedded date to badge; the caller
/// then renders the file name unchanged.
#[must_use]
pub fn split_file_name_for_badge(file_name: &str) -> Option<FileNameDateBadge> {
    let matched = find_date(file_name)?;
    let before = file_name[..matched.start].trim_end_matches(BADGE_SEPARATORS);
    let after = file_name[matched.end..].trim_start_matches(BADGE_SEPARATORS);
    Some(FileNameDateBadge {
        before: before.to_owned(),
        date: matched.date,
        after: after.to_owned(),
    })
}

const WEEKDAY_KANJI: [char; 7] = ['日', '月', '火', '水', '木', '金', '土'];

/// The short badge label for `date` relative to `today`: `本日` when they
/// are the same day, otherwise the shortest form that still disambiguates —
/// day-only within the current month, month/day within the current year,
/// and year/month/day otherwise — each followed by the single-character
/// weekday.
#[must_use]
pub fn format_relative_date_label(date: CalendarDate, today: CalendarDate) -> String {
    if date == today {
        return "本日".to_owned();
    }
    let weekday = WEEKDAY_KANJI[date.weekday_index()];
    if date.year != today.year {
        format!("{}/{}/{}({weekday})", date.year, date.month, date.day)
    } else if date.month != today.month {
        format!("{}/{}({weekday})", date.month, date.day)
    } else {
        format!("{}日({weekday})", date.day)
    }
}

/// Today's calendar date in the terminal's local timezone. The one boundary
/// in this module that reads the system clock; every other function takes
/// "today" as a plain argument so the display rules stay deterministic and
/// testable.
///
/// Uses the platform's own local-time conversion on Unix (macOS and Linux,
/// Hane's shipped and CI-tested targets); other platforms fall back to the
/// UTC calendar date, which only disagrees with the true local date for
/// callers far enough from UTC to be near a day boundary.
#[must_use]
pub fn local_today() -> CalendarDate {
    #[cfg(unix)]
    {
        unix_local_today().unwrap_or_else(utc_today)
    }
    #[cfg(not(unix))]
    {
        utc_today()
    }
}

#[cfg(unix)]
fn unix_local_today() -> Option<CalendarDate> {
    // SAFETY: `now` is a valid `time_t` produced by `libc::time`, and `tm` is
    // a plain-old-data struct of integers that `localtime_r` fully
    // initializes on success; a null return leaves `tm` unused.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        CalendarDate::new(
            tm.tm_year + 1900,
            u32::try_from(tm.tm_mon + 1).ok()?,
            u32::try_from(tm.tm_mday).ok()?,
        )
    }
}

fn utc_today() -> CalendarDate {
    use std::time::{SystemTime, UNIX_EPOCH};

    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() / 86_400);
    civil_from_days(i64::try_from(days).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leap_day_is_valid_only_in_a_leap_year() {
        assert!(CalendarDate::new(2024, 2, 29).is_some());
        assert!(CalendarDate::new(2023, 2, 29).is_none());
        assert!(CalendarDate::new(2000, 2, 29).is_some());
        assert!(CalendarDate::new(1900, 2, 29).is_none());
    }

    #[test]
    fn a_day_past_the_end_of_its_month_is_invalid() {
        assert!(CalendarDate::new(2026, 2, 30).is_none());
        assert!(CalendarDate::new(2026, 4, 31).is_none());
        assert!(CalendarDate::new(2026, 13, 1).is_none());
        assert!(CalendarDate::new(2026, 0, 15).is_none());
    }

    #[test]
    fn known_reference_dates_land_on_their_known_weekday() {
        // 1970-01-01 (the Unix epoch) was a Thursday.
        assert_eq!(CalendarDate::new(1970, 1, 1).unwrap().weekday_index(), 4);
        // 2024-01-01 was a Monday.
        assert_eq!(CalendarDate::new(2024, 1, 1).unwrap().weekday_index(), 1);
    }

    #[test]
    fn a_hyphenated_date_at_the_start_of_the_name_is_found() {
        let badge = split_file_name_for_badge("2026-09-17_ABC.md").unwrap();
        assert_eq!(badge.before, "");
        assert_eq!(badge.date, CalendarDate::new(2026, 9, 17).unwrap());
        assert_eq!(badge.after, "ABC.md");
    }

    #[test]
    fn a_compact_date_at_the_start_of_the_name_is_found() {
        let badge = split_file_name_for_badge("20260917_ABC.md").unwrap();
        assert_eq!(badge.before, "");
        assert_eq!(badge.date, CalendarDate::new(2026, 9, 17).unwrap());
        assert_eq!(badge.after, "ABC.md");
    }

    #[test]
    fn a_hyphenated_date_at_the_end_of_the_name_is_found() {
        let badge = split_file_name_for_badge("XYZ_2026-09-17.md").unwrap();
        assert_eq!(badge.before, "XYZ");
        assert_eq!(badge.date, CalendarDate::new(2026, 9, 17).unwrap());
        assert_eq!(badge.after, ".md");
    }

    #[test]
    fn a_compact_date_at_the_end_of_the_name_is_found() {
        let badge = split_file_name_for_badge("XYZ_20260917.md").unwrap();
        assert_eq!(badge.before, "XYZ");
        assert_eq!(badge.date, CalendarDate::new(2026, 9, 17).unwrap());
        assert_eq!(badge.after, ".md");
    }

    #[test]
    fn a_date_in_the_middle_of_the_name_is_found() {
        let badge = split_file_name_for_badge("Weekly_2026-09-17_Notes.md").unwrap();
        assert_eq!(badge.before, "Weekly");
        assert_eq!(badge.date, CalendarDate::new(2026, 9, 17).unwrap());
        assert_eq!(badge.after, "Notes.md");
    }

    #[test]
    fn a_calendar_invalid_date_is_not_recognized() {
        assert!(split_file_name_for_badge("XYZ_2026-02-30_ABC.md").is_none());
        assert!(split_file_name_for_badge("XYZ_20260230_ABC.md").is_none());
    }

    #[test]
    fn a_file_name_without_a_date_is_not_recognized() {
        assert!(split_file_name_for_badge("Meeting Notes.md").is_none());
    }

    #[test]
    fn a_date_embedded_in_a_longer_digit_run_is_not_recognized() {
        // Both a leading and a trailing extra digit extend the run past a
        // clean 8- or 10-character date token, so neither is a date.
        assert!(split_file_name_for_badge("120260917.md").is_none());
        assert!(split_file_name_for_badge("202609171.md").is_none());
        assert!(split_file_name_for_badge("12026-09-17.md").is_none());
        assert!(split_file_name_for_badge("2026-09-171.md").is_none());
    }

    #[test]
    fn a_run_of_separators_around_the_date_is_trimmed_from_both_sides() {
        let badge = split_file_name_for_badge("XYZ__--2026-09-17--__.md").unwrap();
        assert_eq!(badge.before, "XYZ");
        assert_eq!(badge.after, ".md");
    }

    #[test]
    fn today_is_labeled_as_such() {
        let today = CalendarDate::new(2026, 9, 20).unwrap();
        assert_eq!(format_relative_date_label(today, today), "本日");
    }

    #[test]
    fn a_different_day_in_the_current_month_shows_day_and_weekday() {
        let today = CalendarDate::new(2026, 9, 20).unwrap();
        let date = CalendarDate::new(2026, 9, 17).unwrap();
        assert_eq!(format_relative_date_label(date, today), "17日(木)");
    }

    #[test]
    fn a_different_month_in_the_current_year_shows_month_slash_day() {
        let today = CalendarDate::new(2026, 9, 20).unwrap();
        let date = CalendarDate::new(2026, 10, 3).unwrap();
        assert_eq!(format_relative_date_label(date, today), "10/3(土)");
    }

    #[test]
    fn a_different_year_shows_year_slash_month_slash_day() {
        let today = CalendarDate::new(2026, 9, 20).unwrap();
        let date = CalendarDate::new(2025, 10, 3).unwrap();
        assert_eq!(format_relative_date_label(date, today), "2025/10/3(金)");
    }
}
