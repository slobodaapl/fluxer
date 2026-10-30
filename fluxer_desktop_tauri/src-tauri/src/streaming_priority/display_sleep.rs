// SPDX-License-Identifier: AGPL-3.0-or-later

//! `powerSaveBlocker.start('prevent-display-sleep')`: what Chromium's blocker asks of each
//! platform. Linux: `org.gnome.SessionManager.Inhibit` (idle and suspend flags) where GNOME's
//! session manager answers, else `org.freedesktop.ScreenSaver.Inhibit`; Chromium chose between
//! the two by desktop environment, so the order here is an approximation. The inhibitor lasts
//! while its bus connection does. Windows: a display-required power request. macOS: a
//! no-display-sleep power assertion (not verified on macOS).

/// The reason given to the platform.
const REASON: &str = "Streaming";

#[cfg(target_os = "linux")]
mod platform {
    use zbus::Connection;

    const GNOME_SERVICE: &str = "org.gnome.SessionManager";
    const GNOME_PATH: &str = "/org/gnome/SessionManager";
    /// `INHIBIT_SUSPEND_SESSION | INHIBIT_MARK_SESSION_IDLE`.
    const GNOME_FLAGS: u32 = 4 | 8;
    const SCREENSAVER_SERVICE: &str = "org.freedesktop.ScreenSaver";
    const SCREENSAVER_PATH: &str = "/org/freedesktop/ScreenSaver";

    enum Inhibitor {
        Gnome(u32),
        ScreenSaver(u32),
    }

    pub struct DisplaySleepBlocker {
        connection: Connection,
        inhibitor: Inhibitor,
    }

    async fn call<B>(
        connection: &Connection,
        service: &str,
        path: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        connection
            .call_method(Some(service), path, Some(service), method, body)
            .await
    }

    impl DisplaySleepBlocker {
        pub async fn start(app_name: &str) -> zbus::Result<Self> {
            let connection = Connection::session().await?;
            let gnome = call(
                &connection,
                GNOME_SERVICE,
                GNOME_PATH,
                "Inhibit",
                &(app_name, 0u32, super::REASON, GNOME_FLAGS),
            )
            .await;
            let inhibitor = match gnome {
                Ok(reply) => Inhibitor::Gnome(reply.body().deserialize()?),
                Err(_) => {
                    let reply = call(
                        &connection,
                        SCREENSAVER_SERVICE,
                        SCREENSAVER_PATH,
                        "Inhibit",
                        &(app_name, super::REASON),
                    )
                    .await?;
                    Inhibitor::ScreenSaver(reply.body().deserialize()?)
                }
            };
            Ok(Self {
                connection,
                inhibitor,
            })
        }

        pub async fn stop(self) {
            let released = match self.inhibitor {
                Inhibitor::Gnome(cookie) => {
                    call(&self.connection, GNOME_SERVICE, GNOME_PATH, "Uninhibit", &cookie).await
                }
                Inhibitor::ScreenSaver(cookie) => {
                    call(
                        &self.connection,
                        SCREENSAVER_SERVICE,
                        SCREENSAVER_PATH,
                        "UnInhibit",
                        &cookie,
                    )
                    .await
                }
            };
            if let Err(error) = released {
                tracing::warn!(%error, "[StreamingPriority] Failed to stop power save blocker");
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerSetRequest,
    };
    use windows::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
    use windows::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };
    use windows::core::PWSTR;

    /// The power request handle, kept as an address so the blocker is `Send`.
    pub struct DisplaySleepBlocker(usize);

    impl DisplaySleepBlocker {
        pub async fn start(_app_name: &str) -> windows::core::Result<Self> {
            let mut reason: Vec<u16> = super::REASON.encode_utf16().chain([0]).collect();
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: PWSTR(reason.as_mut_ptr()),
                },
            };
            // SAFETY: `context` and the string it points at outlive the call.
            let request = unsafe { PowerCreateRequest(&context) }?;
            // SAFETY: `request` is the power request just created.
            if let Err(error) = unsafe { PowerSetRequest(request, PowerRequestDisplayRequired) } {
                // SAFETY: as above; nothing else holds it.
                let _ = unsafe { CloseHandle(request) };
                return Err(error);
            }
            Ok(Self(request.0 as usize))
        }

        pub async fn stop(self) {
            let request = HANDLE(self.0 as *mut core::ffi::c_void);
            // SAFETY: `request` is the power request `start` created and set; it is closed once.
            unsafe {
                if let Err(error) = PowerClearRequest(request, PowerRequestDisplayRequired) {
                    tracing::warn!(%error, "[StreamingPriority] Failed to stop power save blocker");
                }
                let _ = CloseHandle(request);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    type CFStringRef = *const c_void;

    const ASSERTION_TYPE: &str = "NoDisplaySleepAssertion";
    /// `kIOPMAssertionLevelOn`.
    const ASSERTION_LEVEL_ON: u32 = 255;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CFStringRef;
        fn CFRelease(value: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    fn cf_string(value: &str) -> CFStringRef {
        // SAFETY: the bytes are valid UTF-8 of the given length.
        unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                value.as_ptr(),
                value.len() as isize,
                UTF8,
                0,
            )
        }
    }

    pub struct DisplaySleepBlocker(u32);

    impl DisplaySleepBlocker {
        pub async fn start(_app_name: &str) -> Result<Self, String> {
            let assertion_type = cf_string(ASSERTION_TYPE);
            let name = cf_string(super::REASON);
            let mut id = 0u32;
            // SAFETY: both strings are live CFStrings and `id` outlives the call.
            let status =
                unsafe { IOPMAssertionCreateWithName(assertion_type, ASSERTION_LEVEL_ON, name, &mut id) };
            // SAFETY: both strings were created above and are released once.
            unsafe {
                CFRelease(assertion_type);
                CFRelease(name);
            }
            if status == 0 {
                Ok(Self(id))
            } else {
                Err(format!("IOPMAssertionCreateWithName returned {status}"))
            }
        }

        pub async fn stop(self) {
            // SAFETY: the assertion `start` created, released once.
            let status = unsafe { IOPMAssertionRelease(self.0) };
            if status != 0 {
                tracing::warn!(status, "[StreamingPriority] Failed to stop power save blocker");
            }
        }
    }
}

pub use self::platform::DisplaySleepBlocker;
