//! System notifications. The app shows one only when the owner must act:
//! reconnect Square, label writes stopped, or checks failing for days.
//!
//! - macOS: `UNUserNotificationCenter`. Permission is asked once, from the
//!   window during setup, so a background check never triggers the prompt.
//! - Windows: a toast under the app's AppUserModelID, registered per user.
//! - Linux (development only): written to stderr.

use std::sync::Mutex;

pub const AUMID: &str = "com.tightline.deadstockfinder";
pub const APP_NAME: &str = "Dead Stock Finder";

pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

/// The operating system's notifications.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&self, title: &str, body: &str) {
        os::notify(title, body);
    }
}

/// Keeps notifications in memory, for tests and the self-test.
#[derive(Default)]
pub struct Recorder(pub Mutex<Vec<(String, String)>>);

impl Notifier for Recorder {
    fn notify(&self, title: &str, body: &str) {
        self.0.lock().unwrap().push((title.to_string(), body.to_string()));
    }
}

impl Recorder {
    pub fn taken(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Asks the operating system for permission to show notifications, from the
/// window (never from a background check). Returns whether they're allowed;
/// `None` where there is nothing to ask.
pub fn request_permission() -> Option<bool> {
    os::request_permission()
}

#[cfg(target_os = "macos")]
mod os {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
    };

    /// UNUserNotificationCenter raises an exception outside an app bundle.
    fn in_bundle() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    pub fn notify(title: &str, body: &str) {
        if !in_bundle() {
            eprintln!("notification: {title}: {body}");
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let id = NSString::from_str(&format!("dsf-{}", title.len() + body.len()));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        let (tx, rx) = mpsc::channel::<()>();
        let block = RcBlock::new(move |_error: *mut NSError| {
            let _ = tx.send(());
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
        // A background check exits right after; let the request land first.
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }

    pub fn request_permission() -> Option<bool> {
        if !in_bundle() {
            return None;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let (tx, rx) = mpsc::channel::<bool>();
        let block = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let _ = tx.send(granted.as_bool());
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &block,
        );
        rx.recv_timeout(Duration::from_secs(120)).ok()
    }
}

#[cfg(windows)]
mod os {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegSetValueExW,
    };

    use super::{APP_NAME, AUMID};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Unpackaged apps name themselves to the toast system per user:
    /// HKCU\Software\Classes\AppUserModelId\<AUMID>\DisplayName.
    fn register() {
        let path = wide(&format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
        let mut key: HKEY = std::ptr::null_mut();
        let ok = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            return;
        }
        let name = wide("DisplayName");
        let value = wide(APP_NAME);
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            );
            RegCloseKey(key);
        }
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    pub fn notify(title: &str, body: &str) {
        register();
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape(title),
            escape(body)
        );
        let shown = (|| -> windows::core::Result<()> {
            let doc = XmlDocument::new()?;
            doc.LoadXml(&HSTRING::from(xml))?;
            let toast = ToastNotification::CreateToastNotification(&doc)?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
        })();
        if let Err(e) = shown {
            eprintln!("notification failed: {e}");
        }
    }

    pub fn request_permission() -> Option<bool> {
        register();
        None
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    pub fn notify(title: &str, body: &str) {
        eprintln!("notification: {title}: {body}");
    }

    pub fn request_permission() -> Option<bool> {
        None
    }
}
