// SPDX-License-Identifier: AGPL-3.0-or-later

//! The macOS Focus check of NotificationState.ts (`getMacosNotificationSoundPolicy`), read from
//! the Do Not Disturb store in `~/Library/DoNotDisturb/DB`:
//!
//! - `Assertions.json` lists the Focus modes switched on by hand;
//! - `ModeConfigurations.json` holds each mode's time triggers, which switch it on by schedule;
//! - `ModeConfigurationsSecure.json` lists the apps each mode allows.
//!
//! No assertion and no active mode allows the sound; assertions without a mode id, or an
//! active mode that does not allow this app, suppress it. The app is the bundle identifier,
//! [`BuildChannel::desktop_id`], where Electron used its own `MACOS_BUNDLE_ID`. Not verified on
//! macOS.

use serde_json::{Map, Value};

#[cfg(target_os = "macos")]
use crate::build_channel::BuildChannel;

/// Reads the store now. Missing or unparsable files count as absent.
#[cfg(target_os = "macos")]
pub fn allows_now() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return true;
    };
    let directory = std::path::Path::new(&home).join("Library/DoNotDisturb/DB");
    let read = |name: &str| {
        std::fs::read_to_string(directory.join(name))
            .ok()
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
    };
    allows(
        read("Assertions.json").as_ref(),
        read("ModeConfigurations.json").as_ref(),
        || read("ModeConfigurationsSecure.json"),
        local_minutes_of_day(),
        BuildChannel::current().desktop_id(),
    )
}

/// `now.getHours() * 60 + now.getMinutes()` in local time.
#[cfg(target_os = "macos")]
fn local_minutes_of_day() -> f64 {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `now` and `local` are valid for the call; `localtime_r` writes only `local`.
    if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
        return 0.0;
    }
    f64::from(local.tm_hour * 60 + local.tm_min)
}

/// The policy over the parsed store files; `secure` is read only when a mode is active.
fn allows(
    assertions: Option<&Value>,
    mode_configurations: Option<&Value>,
    secure: impl FnOnce() -> Option<Value>,
    minutes_of_day: f64,
    bundle_id: &str,
) -> bool {
    let records = first_data_record(assertions)
        .and_then(|record| record.get("storeAssertionRecords"))
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(Value::as_object)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut active_modes: Vec<&str> = Vec::new();
    fn add<'a>(modes: &mut Vec<&'a str>, mode: &'a str) {
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    for record in &records {
        if let Some(mode) = record
            .get("assertionDetails")
            .and_then(Value::as_object)
            .and_then(|details| details.get("assertionDetailsModeIdentifier"))
            .and_then(Value::as_str)
            .filter(|mode| !mode.is_empty())
        {
            add(&mut active_modes, mode);
        }
    }
    if let Some(configurations) = first_data_record(mode_configurations)
        .and_then(|record| record.get("modeConfigurations"))
        .and_then(Value::as_object)
    {
        for (mode, configuration) in configurations {
            if configuration
                .as_object()
                .is_some_and(|configuration| has_active_schedule(configuration, minutes_of_day))
            {
                add(&mut active_modes, mode);
            }
        }
    }
    if records.is_empty() && active_modes.is_empty() {
        return true;
    }
    if active_modes.is_empty() {
        return false;
    }
    let secure = secure();
    active_modes
        .iter()
        .all(|mode| is_bundle_allowed(secure.as_ref(), mode, bundle_id))
}

/// `firstDataRecord`: `value.data[0]` when it is an object.
fn first_data_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.get("data")?.as_array()?.first()?.as_object()
}

/// `readNumber`.
fn number(value: Option<&Value>) -> Option<f64> {
    value?.as_f64().filter(|number| number.is_finite())
}

/// `hasActiveMacosFocusSchedule` with `isCurrentTimeInMacosFocusTrigger`.
fn has_active_schedule(configuration: &Map<String, Value>, minutes_of_day: f64) -> bool {
    let Some(triggers) = configuration
        .get("triggers")
        .and_then(Value::as_object)
        .and_then(|container| container.get("triggers"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    triggers.iter().filter_map(Value::as_object).any(|trigger| {
        if number(trigger.get("enabledSetting")) != Some(2.0) {
            return false;
        }
        let (Some(start_hour), Some(end_hour)) = (
            number(trigger.get("timePeriodStartTimeHour")),
            number(trigger.get("timePeriodEndTimeHour")),
        ) else {
            return false;
        };
        let start =
            start_hour * 60.0 + number(trigger.get("timePeriodStartTimeMinute")).unwrap_or(0.0);
        let end = end_hour * 60.0 + number(trigger.get("timePeriodEndTimeMinute")).unwrap_or(0.0);
        if start < end {
            minutes_of_day >= start && minutes_of_day < end
        } else if end < start {
            minutes_of_day >= start || minutes_of_day < end
        } else {
            false
        }
    })
}

/// `isMacosBundleAllowedInFocus` with `isAllowedMacosApplicationKey`.
fn is_bundle_allowed(secure: Option<&Value>, mode: &str, bundle_id: &str) -> bool {
    if mode == "NONE" {
        return false;
    }
    first_data_record(secure)
        .and_then(|record| record.get("secureModeConfigurations"))
        .and_then(Value::as_object)
        .and_then(|configurations| configurations.get(mode))
        .and_then(Value::as_object)
        .and_then(|configuration| configuration.get("secureConfiguration"))
        .and_then(Value::as_object)
        .and_then(|configuration| configuration.get("allowedApplications"))
        .and_then(Value::as_object)
        .is_some_and(|applications| {
            applications.keys().any(|key| {
                key == bundle_id
                    || key
                        .strip_prefix(bundle_id)
                        .is_some_and(|rest| rest.starts_with('.'))
                    || bundle_id
                        .strip_prefix(key.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BUNDLE: &str = "app.fluxer.tauri.development";

    fn assertion(mode: Option<&str>) -> Value {
        let details = match mode {
            Some(mode) => json!({"assertionDetailsModeIdentifier": mode}),
            None => json!({}),
        };
        json!({"data": [{"storeAssertionRecords": [{"assertionDetails": details}]}]})
    }

    fn secure(mode: &str, allowed: &[&str]) -> Value {
        let applications: Map<String, Value> = allowed
            .iter()
            .map(|key| ((*key).to_owned(), json!({})))
            .collect();
        json!({"data": [{"secureModeConfigurations": {
            mode: {"secureConfiguration": {"allowedApplications": applications}}
        }}]})
    }

    fn schedule(mode: &str, start: (u32, u32), end: (u32, u32)) -> Value {
        json!({"data": [{"modeConfigurations": {mode: {"triggers": {"triggers": [{
            "enabledSetting": 2,
            "timePeriodStartTimeHour": start.0,
            "timePeriodStartTimeMinute": start.1,
            "timePeriodEndTimeHour": end.0,
            "timePeriodEndTimeMinute": end.1,
        }]}}}}]})
    }

    fn decide(
        assertions: Option<Value>,
        modes: Option<Value>,
        secure: Option<Value>,
        minutes: f64,
    ) -> bool {
        allows(
            assertions.as_ref(),
            modes.as_ref(),
            || secure,
            minutes,
            BUNDLE,
        )
    }

    #[test]
    fn no_focus_allows_the_sound() {
        assert!(decide(None, None, None, 600.0));
        assert!(decide(
            Some(json!({"data": []})),
            Some(json!({})),
            None,
            600.0
        ));
    }

    #[test]
    fn a_manual_focus_suppresses_unless_it_allows_the_app() {
        let sleep = "com.apple.sleep.sleep-mode";
        assert!(!decide(Some(assertion(Some(sleep))), None, None, 600.0));
        assert!(!decide(
            Some(assertion(Some(sleep))),
            None,
            Some(secure(sleep, &["com.other"])),
            600.0
        ));
        assert!(decide(
            Some(assertion(Some(sleep))),
            None,
            Some(secure(sleep, &[BUNDLE])),
            600.0
        ));
        // A key and the bundle id that are prefixes of each other at a dot boundary match.
        assert!(decide(
            Some(assertion(Some(sleep))),
            None,
            Some(secure(sleep, &["app.fluxer.tauri"])),
            600.0
        ));
        assert!(!decide(
            Some(assertion(Some(sleep))),
            None,
            Some(secure(sleep, &["app.fluxer.taur"])),
            600.0
        ));
        // An assertion without a mode id cannot be checked against an allow list.
        assert!(!decide(
            Some(assertion(None)),
            None,
            Some(secure(sleep, &[BUNDLE])),
            600.0
        ));
        assert!(!decide(
            Some(assertion(Some("NONE"))),
            None,
            Some(secure("NONE", &[BUNDLE])),
            600.0
        ));
    }

    #[test]
    fn scheduled_focus_applies_inside_its_window_including_past_midnight() {
        let night = "night";
        let overnight = schedule(night, (23, 0), (7, 30));
        assert!(!decide(None, Some(overnight.clone()), None, 60.0));
        assert!(!decide(None, Some(overnight.clone()), None, 23.0 * 60.0));
        assert!(decide(
            None,
            Some(overnight.clone()),
            None,
            7.0 * 60.0 + 30.0
        ));
        assert!(decide(
            None,
            Some(overnight.clone()),
            Some(secure(night, &[BUNDLE])),
            60.0
        ));
        let work = schedule("work", (9, 0), (17, 0));
        assert!(!decide(None, Some(work.clone()), None, 9.0 * 60.0));
        assert!(decide(None, Some(work), None, 17.0 * 60.0));
        assert!(decide(
            None,
            Some(schedule("never", (9, 0), (9, 0))),
            None,
            9.0 * 60.0
        ));
    }
}
