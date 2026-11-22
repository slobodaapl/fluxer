// SPDX-License-Identifier: AGPL-3.0-or-later
//! Timestamp style skeletons mirror packages/date_utils/DateTimestampStyle.ts;
//! relative units mirror markdown/DateFormatter.ts. ICU supplies the same CLDR
//! grammar, hour cycles and ordering used by Intl rather than hand translations.
use fluxer_markdown_parser::ast::TimestampStyle;
use serde_json::Value;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub range: Range<usize>,
    pub seconds: i64,
    pub style: TimestampStyle,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub locale: String,
    pub hour12: Option<bool>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            locale: "en-US".into(),
            hour12: None,
        }
    }
}
impl Preferences {
    pub fn from_settings(settings: &Value) -> Self {
        let locale = settings
            .get("locale")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 100 && !s.contains('\0'))
            .unwrap_or("en-US")
            .to_owned();
        let hour12 = match settings
            .get("time_format")
            .or_else(|| settings.get("timeFormat"))
            .and_then(Value::as_u64)
        {
            Some(1) => Some(true),
            Some(2) => Some(false),
            _ => None,
        };
        Self { locale, hour12 }
    }
}
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn relative_unit(seconds: i64, current: i64) -> (f64, i32) {
    let delta = seconds.saturating_sub(current);
    let absolute = delta.unsigned_abs();
    // ICU URelativeDateTimeUnit: year=0, month=2, week=3, day=4,
    // hour=5, minute=6, second=7. Source intentionally uses fixed 365/30 days.
    let (size, unit) = [
        (31_536_000, 0),
        (2_592_000, 2),
        (604_800, 3),
        (86_400, 4),
        (3_600, 5),
        (60, 6),
        (1, 7),
    ]
    .into_iter()
    .find(|(size, _)| absolute >= *size)
    .unwrap_or((1, 7));
    (
        (absolute / size) as f64 * if delta < 0 { -1. } else { 1. },
        unit,
    )
}
fn skeleton(style: TimestampStyle, hour12: Option<bool>) -> String {
    use TimestampStyle::*;
    let hour = match hour12 {
        Some(true) => "h",
        Some(false) => "H",
        None => "j",
    };
    match style {
        ShortTime => format!("{hour}m"),
        LongTime => format!("{hour}ms"),
        ShortDate => "yMd".into(),
        LongDate => "yMMMMd".into(),
        ShortDateTime => format!("yMMMMd{hour}m"),
        LongDateTime => format!("yMMMMEEEEd{hour}m"),
        ShortDateShortTime => format!("yMd{hour}m"),
        ShortDateMediumTime => format!("yMd{hour}ms"),
        RelativeTime => String::new(),
    }
}
/// Bounded label cache avoids ICU work for static labels on every paint pass.
pub fn format(seconds: i64, style: TimestampStyle, prefs: &Preferences, current: i64) -> String {
    type Key = (i64, u8, String, Option<bool>, i64);
    static LABELS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<Key, String>>> =
        std::sync::OnceLock::new();
    let key = (
        seconds,
        style as u8,
        prefs.locale.clone(),
        prefs.hour12,
        if style == TimestampStyle::RelativeTime {
            current
        } else {
            0
        },
    );
    let cache = LABELS.get_or_init(Default::default);
    if let Ok(labels) = cache.lock()
        && let Some(label) = labels.get(&key)
    {
        return label.clone();
    }
    let label = format_uncached(seconds, style, prefs, current);
    if let Ok(mut labels) = cache.lock() {
        if labels.len() >= 512 {
            labels.clear();
        }
        labels.insert(key, label.clone());
    }
    label
}
fn format_uncached(
    seconds: i64,
    style: TimestampStyle,
    prefs: &Preferences,
    current: i64,
) -> String {
    if seconds.unsigned_abs() > 8_640_000_000_000 {
        return seconds.to_string();
    }
    if style == TimestampStyle::RelativeTime {
        let (value, unit) = relative_unit(seconds, current);
        if let Some(text) = icu::relative(&prefs.locale, value, unit) {
            return text;
        }
        let name = [
            "year", "quarter", "month", "week", "day", "hour", "minute", "second",
        ][unit as usize];
        if value == 0. {
            return "now".into();
        }
        let amount = format!(
            "{} {name}{}",
            value.abs(),
            if value.abs() == 1. { "" } else { "s" }
        );
        return if value < 0. {
            format!("{amount} ago")
        } else {
            format!("in {amount}")
        };
    }
    let skeleton = skeleton(style, prefs.hour12);
    icu::date(&prefs.locale, seconds, &skeleton).unwrap_or_else(|| {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_opt(seconds, 0)
            .single()
            .map(|date| date.format("%Y-%m-%d %H:%M:%S %Z").to_string())
            .unwrap_or_else(|| seconds.to_string())
    })
}
fn tooltip_relative(seconds: i64, current: i64, prefs: &Preferences) -> String {
    use chrono::{Datelike, Days, Months, TimeZone};
    let mut unit = None;
    if let (Some(target), Some(base)) = (
        chrono::Local.timestamp_opt(seconds, 0).single(),
        chrono::Local.timestamp_opt(current, 0).single(),
    ) {
        let future = seconds > current;
        let months = ((target.year() as i64 - base.year() as i64) * 12 + target.month0() as i64
            - base.month0() as i64)
            .unsigned_abs()
            .min(u32::MAX as u64) as u32;
        let anchor = if future {
            base.checked_add_months(Months::new(months))
        } else {
            base.checked_sub_months(Months::new(months))
        };
        let months = months.saturating_sub(u32::from(anchor.is_some_and(|anchor| {
            if future {
                anchor > target
            } else {
                anchor < target
            }
        })));
        let direction = if future { 1. } else { -1. };
        if months >= 12 {
            unit = Some(((months / 12) as f64 * direction, 0));
        } else if months > 0 {
            unit = Some((months as f64 * direction, 2));
        } else {
            let days = (target.date_naive() - base.date_naive())
                .num_days()
                .unsigned_abs();
            let anchor = if future {
                base.checked_add_days(Days::new(days))
            } else {
                base.checked_sub_days(Days::new(days))
            };
            let days = days.saturating_sub(u64::from(anchor.is_some_and(|anchor| {
                if future {
                    anchor > target
                } else {
                    anchor < target
                }
            })));
            if days > 0 {
                unit = Some((days as f64 * direction, 4));
            }
        }
    }
    let (mut value, mut kind) = unit.unwrap_or_else(|| relative_unit(seconds, current));
    if kind == 3 {
        kind = 4;
        value = (seconds - current).unsigned_abs() as f64 / 86_400.;
        value = value.trunc() * if seconds > current { 1. } else { -1. };
    }
    if value == 0. && seconds <= current {
        value = -0.;
    }
    icu::relative_numeric(&prefs.locale, value, kind)
        .unwrap_or_else(|| format(seconds, TimestampStyle::RelativeTime, prefs, current))
}
pub fn tooltip(timestamp: &Timestamp, prefs: &Preferences, current: i64) -> String {
    // TimestampRenderer's full tooltip uses locale-default hour cycle, separate
    // from the account's explicitly chosen widget hour cycle.
    let full = icu::date(&prefs.locale, timestamp.seconds, "yMMMMdEEEE")
        .zip(icu::date(&prefs.locale, timestamp.seconds, "jms"))
        .map(|(date, time)| format!("{date} {time}"))
        .unwrap_or_else(|| {
            format(
                timestamp.seconds,
                TimestampStyle::LongDateTime,
                prefs,
                current,
            )
        });
    format!(
        "{full}\n{}",
        tooltip_relative(timestamp.seconds, current, prefs)
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod icu {
    use std::{
        ffi::{CString, c_char, c_void},
        sync::OnceLock,
    };
    type OpenPattern = unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void;
    type BestPattern =
        unsafe extern "C" fn(*mut c_void, *const u16, i32, *mut u16, i32, *mut i32) -> i32;
    type Close = unsafe extern "C" fn(*mut c_void);
    type OpenDate = unsafe extern "C" fn(
        i32,
        i32,
        *const c_char,
        *const u16,
        i32,
        *const u16,
        i32,
        *mut i32,
    ) -> *mut c_void;
    type FormatDate =
        unsafe extern "C" fn(*const c_void, f64, *mut u16, i32, *mut c_void, *mut i32) -> i32;
    type OpenRelative =
        unsafe extern "C" fn(*const c_char, *mut c_void, i32, i32, *mut i32) -> *mut c_void;
    type FormatRelative =
        unsafe extern "C" fn(*const c_void, f64, i32, *mut u16, i32, *mut i32) -> i32;
    struct Api {
        pattern_open: OpenPattern,
        pattern_best: BestPattern,
        pattern_close: Close,
        date_open: OpenDate,
        date_format: FormatDate,
        date_close: Close,
        relative_open: OpenRelative,
        relative_format: FormatRelative,
        relative_numeric: FormatRelative,
        relative_close: Close,
    }
    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }
    #[cfg(target_os = "macos")]
    fn open_library() -> *mut c_void {
        unsafe { dlopen(c"/usr/lib/libicucore.A.dylib".as_ptr(), 1) }
    }
    #[cfg(target_os = "linux")]
    fn open_library() -> *mut c_void {
        let handle = unsafe { dlopen(c"libicui18n.so".as_ptr(), 1) };
        if !handle.is_null() {
            return handle;
        }
        for version in (60..=100).rev() {
            let library = CString::new(format!("libicui18n.so.{version}")).unwrap();
            let handle = unsafe { dlopen(library.as_ptr(), 1) };
            if !handle.is_null() {
                return handle;
            }
        }
        std::ptr::null_mut()
    }
    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            // ICU C exports are version-suffixed on Linux. Resolve every symbol
            // as one version before accepting it. Keep the successful DSO open
            // for process lifetime; no ICU object/pointer is shared across threads.
            let handle = open_library();
            if handle.is_null() {
                return None;
            }
            let suffixes = std::iter::once(String::new());
            #[cfg(target_os = "linux")]
            let suffixes = suffixes.chain((60..=100).rev().map(|n| format!("_{n}")));
            for suffix in suffixes {
                macro_rules! symbol {
                    ($name:literal, $ty:ty) => {{
                        let name = CString::new(format!("{}{}", $name, suffix)).ok()?;
                        let ptr = dlsym(handle, name.as_ptr());
                        if ptr.is_null() {
                            None
                        } else {
                            Some(std::mem::transmute::<*mut c_void, $ty>(ptr))
                        }
                    }};
                }
                let candidate = (|| {
                    Some(Api {
                        pattern_open: symbol!("udatpg_open", OpenPattern)?,
                        pattern_best: symbol!("udatpg_getBestPattern", BestPattern)?,
                        pattern_close: symbol!("udatpg_close", Close)?,
                        date_open: symbol!("udat_open", OpenDate)?,
                        date_format: symbol!("udat_format", FormatDate)?,
                        date_close: symbol!("udat_close", Close)?,
                        relative_open: symbol!("ureldatefmt_open", OpenRelative)?,
                        relative_format: symbol!("ureldatefmt_format", FormatRelative)?,
                        relative_numeric: symbol!("ureldatefmt_formatNumeric", FormatRelative)?,
                        relative_close: symbol!("ureldatefmt_close", Close)?,
                    })
                })();
                if candidate.is_some() {
                    return candidate;
                }
            }
            dlclose(handle);
            None
        })
        .as_ref()
    }
    fn result(buffer: &[u16], length: i32, status: i32) -> Option<String> {
        if status > 0 || length <= 0 || length as usize >= buffer.len() {
            return None;
        }
        String::from_utf16(&buffer[..length as usize]).ok()
    }
    pub(super) fn date(locale: &str, seconds: i64, skeleton: &str) -> Option<String> {
        let api = api()?;
        let locale = CString::new(locale).ok()?;
        let skeleton: Vec<_> = skeleton.encode_utf16().collect();
        let mut status = 0;
        let mut pattern = [0u16; 256];
        let mut buffer = [0u16; 512];
        unsafe {
            let generator = (api.pattern_open)(locale.as_ptr(), &mut status);
            if generator.is_null() {
                return None;
            }
            let length = (api.pattern_best)(
                generator,
                skeleton.as_ptr(),
                skeleton.len() as i32,
                pattern.as_mut_ptr(),
                256,
                &mut status,
            );
            (api.pattern_close)(generator);
            if status > 0 || length <= 0 || length >= 256 {
                return None;
            }
            let date = (api.date_open)(
                -2,
                -2,
                locale.as_ptr(),
                std::ptr::null(),
                0,
                pattern.as_ptr(),
                length,
                &mut status,
            );
            if date.is_null() {
                return None;
            }
            let length = (api.date_format)(
                date,
                seconds as f64 * 1000.,
                buffer.as_mut_ptr(),
                512,
                std::ptr::null_mut(),
                &mut status,
            );
            (api.date_close)(date);
            result(&buffer, length, status)
        }
    }
    pub(super) fn relative(locale: &str, value: f64, unit: i32) -> Option<String> {
        relative_with_mode(locale, value, unit, false)
    }
    pub(super) fn relative_numeric(locale: &str, value: f64, unit: i32) -> Option<String> {
        relative_with_mode(locale, value, unit, true)
    }
    fn relative_with_mode(locale: &str, value: f64, unit: i32, numeric: bool) -> Option<String> {
        let api = api()?;
        let locale = CString::new(locale).ok()?;
        let mut status = 0;
        let mut buffer = [0u16; 256];
        unsafe {
            let formatter =
                (api.relative_open)(locale.as_ptr(), std::ptr::null_mut(), 0, 0x100, &mut status);
            if formatter.is_null() {
                return None;
            }
            let format = if numeric {
                api.relative_numeric
            } else {
                api.relative_format
            };
            let length = format(
                formatter,
                value,
                unit,
                buffer.as_mut_ptr(),
                256,
                &mut status,
            );
            (api.relative_close)(formatter);
            result(&buffer, length, status)
        }
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod icu {
    pub(super) fn relative_numeric(_: &str, _: f64, _: i32) -> Option<String> {
        None
    }
    pub(super) fn date(_: &str, _: i64, _: &str) -> Option<String> {
        None
    }
    pub(super) fn relative(_: &str, _: f64, _: i32) -> Option<String> {
        None
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    #[test]
    fn relative_uses_source_floor_thresholds_and_calendar_grammar() {
        let prefs = Preferences::default();
        for (offset, label) in [
            (0, "now"),
            (59, "in 59 seconds"),
            (60, "in 1 minute"),
            (-86_400, "yesterday"),
            (604_800, "next week"),
            (2_592_000, "next month"),
            (31_536_000, "next year"),
        ] {
            assert_eq!(
                format(
                    1_000_000 + offset,
                    TimestampStyle::RelativeTime,
                    &prefs,
                    1_000_000
                ),
                label
            );
        }
    }
    #[test]
    fn icu_styles_respect_locale_and_hour_cycle() {
        let twelve =
            Preferences::from_settings(&serde_json::json!({"locale":"en-US","time_format":1}));
        let twenty_four =
            Preferences::from_settings(&serde_json::json!({"locale":"en-US","time_format":2}));
        let value = 1_700_000_000;
        assert!(format(value, TimestampStyle::LongTime, &twelve, value).contains(['A', 'P']));
        assert!(!format(value, TimestampStyle::LongTime, &twenty_four, value).contains(['A', 'P']));
        let french =
            Preferences::from_settings(&serde_json::json!({"locale":"fr","time_format":2}));
        assert!(format(value, TimestampStyle::LongDate, &french, value).contains("novembre"));
        assert!(format(value, TimestampStyle::LongDate, &twelve, value).contains("November"));
        assert_eq!(
            format(value + 86_400, TimestampStyle::RelativeTime, &french, value),
            "demain"
        );
    }
}
