//! What starts the headless check, and what keeps two checks from running
//! at once.
//!
//! **macOS.** Two LaunchAgents ship inside the app bundle
//! (`Contents/Library/LaunchAgents/`):
//! - `…signin` runs at load, which is every sign-in after a boot or restart;
//! - `…daily` fires every 15 minutes, and the check runs only when the
//!   owner's daily time has passed since the last one. launchd runs a time
//!   missed during sleep once on wake.
//!
//! A ticking agent instead of one at the exact time, because the plist is
//! inside the signed bundle and can't be rewritten when the owner changes the
//! time. macOS 13+ registers both with `SMAppService` (listed under Login
//! Items); macOS 11–12 copies them to `~/Library/LaunchAgents` and loads them
//! with `launchctl`.
//!
//! **Windows.** One per-user Task Scheduler task with two triggers (at log on
//! of this user, and daily at the chosen time), created with `schtasks` as
//! the signed-in user, no admin. If that's refused, a daily-only task plus a
//! per-user Run entry for sign-in; if no task can be made, the Run entry
//! alone. The registration records which path was used.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

pub const SIGNIN_LABEL: &str = "com.tightline.deadstockfinder.signin";
pub const DAILY_LABEL: &str = "com.tightline.deadstockfinder.daily";
/// The task sits in Task Scheduler's root, named for the Windows user:
/// standard users may create tasks there but not new folders, and two
/// people on one PC each get their own.
pub fn task_name(user: &str) -> String {
    let clean: String =
        user.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    format!("\\Dead Stock Finder check ({clean})")
}
pub const RUN_VALUE: &str = "DeadStockFinderCheck";
pub const TICK_MINUTES: [u32; 4] = [0, 15, 30, 45];

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("{0}")]
    Failed(String),
    #[error("background checks aren't available on this system")]
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// macOS 13+: both agents registered with SMAppService.
    SmAppService,
    /// macOS 11–12: plists in ~/Library/LaunchAgents, loaded with launchctl.
    LaunchAgentsFolder,
    /// Windows: one task, sign-in and daily triggers.
    TaskScheduler,
    /// Windows: daily task, plus the Run entry for sign-in.
    TaskSchedulerDailyAndRunKey,
    /// Windows: Run entry only; the daily check happens at the next sign-in.
    RunKeyOnly,
    /// Nothing registered.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub method: Method,
    /// What the system says, for the log and the status screen (for
    /// example "enabled", or "requires approval in Login Items").
    pub detail: String,
}

/// How the check was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Sign-in after a boot, restart or log out.
    Signin,
    /// The daily time (Windows), or any background start that should run.
    Scheduled,
    /// The 15-minute tick (macOS): runs only when the daily check is due.
    Tick,
    /// Check now, in the window.
    Manual,
    /// `--self-test`.
    SelfTest,
}

impl Trigger {
    pub fn parse(s: &str) -> Option<Trigger> {
        Some(match s {
            "signin" => Trigger::Signin,
            "scheduled" | "daily" => Trigger::Scheduled,
            "tick" => Trigger::Tick,
            "manual" => Trigger::Manual,
            "self-test" => Trigger::SelfTest,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Trigger::Signin => "signin",
            Trigger::Scheduled => "scheduled",
            Trigger::Tick => "tick",
            Trigger::Manual => "manual",
            Trigger::SelfTest => "self-test",
        }
    }
}

/// The most recent occurrence of the daily time at or before `now`, in the
/// computer's time zone. A time that doesn't exist on a spring-forward day
/// moves to the first valid minute after it.
pub fn last_occurrence<Tz: TimeZone>(tz: &Tz, now: DateTime<Utc>, hour: u8, minute: u8) -> DateTime<Utc> {
    let local_now = now.with_timezone(tz);
    let time = NaiveTime::from_hms_opt(u32::from(hour.min(23)), u32::from(minute.min(59)), 0).unwrap();
    let mut day = local_now.date_naive();
    loop {
        let mut naive = day.and_time(time);
        let at = loop {
            match tz.from_local_datetime(&naive).earliest() {
                Some(t) => break t.with_timezone(&Utc),
                None => naive += Duration::minutes(1),
            }
        };
        if at <= now {
            return at;
        }
        day = day.pred_opt().unwrap();
    }
}

/// Whether a 15-minute tick should run the check: the daily time has passed
/// since the last attempt. After a failed attempt, ticks retry at most every
/// two hours until one succeeds.
pub fn tick_due(
    now: DateTime<Utc>,
    check_time: (u8, u8),
    last_attempt: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
) -> bool {
    tick_due_in(&Local, now, check_time, last_attempt, last_success)
}

pub fn tick_due_in<Tz: TimeZone>(
    tz: &Tz,
    now: DateTime<Utc>,
    check_time: (u8, u8),
    last_attempt: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
) -> bool {
    let occurrence = last_occurrence(tz, now, check_time.0, check_time.1);
    if last_success.is_some_and(|s| s >= occurrence) {
        return false;
    }
    match last_attempt {
        None => true,
        Some(a) if a < occurrence => true,
        Some(a) => now - a >= Duration::hours(2),
    }
}

/// The single-instance lock: held for the whole check, released when
/// dropped (or when the process ends, however it ends).
pub struct CheckLock {
    _file: File,
    pub path: PathBuf,
}

/// Takes the lock, or returns `None` if another check holds it.
pub fn try_lock(dir: &Path) -> std::io::Result<Option<CheckLock>> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("check.lock");
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(CheckLock { _file: file, path })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// The LaunchAgent plist. `program` is either a path inside the bundle
/// (`BundleProgram`, for SMAppService) or an absolute path (legacy folder).
pub fn launch_agent_plist(label: &str, program: &str, bundled: bool, trigger: Trigger) -> String {
    let program_key = if bundled { "BundleProgram" } else { "Program" };
    let schedule = match trigger {
        Trigger::Tick => {
            let entries: String = TICK_MINUTES
                .iter()
                .map(|m| {
                    format!(
                        "\t\t<dict>\n\t\t\t<key>Minute</key>\n\t\t\t<integer>{m}</integer>\n\t\t</dict>\n"
                    )
                })
                .collect();
            format!(
                "\t<key>RunAtLoad</key>\n\t<false/>\n\t<key>StartCalendarInterval</key>\n\t<array>\n{entries}\t</array>\n"
            )
        }
        _ => "\t<key>RunAtLoad</key>\n\t<true/>\n".to_string(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>Label</key>\n\t<string>{label}</string>\n\
\t<key>{program_key}</key>\n\t<string>{program}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{program}</string>\n\t\t<string>--check</string>\n\t\t<string>--trigger</string>\n\t\t<string>{trigger}</string>\n\t</array>\n\
\t<key>AssociatedBundleIdentifiers</key>\n\t<string>com.tightline.deadstockfinder</string>\n\
\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n\
\t<key>ProcessType</key>\n\t<string>Background</string>\n\
{schedule}</dict>\n</plist>\n",
        trigger = trigger.name(),
    )
}

/// Path of the executable inside the bundle, as BundleProgram wants it.
pub const BUNDLE_PROGRAM: &str = "Contents/MacOS/dead-stock-finder";

/// The Windows task definition (Task Scheduler XML).
pub fn task_xml(user: &str, exe: &str, check_time: (u8, u8), with_logon: bool) -> String {
    let name = task_name(user.rsplit('\\').next().unwrap_or(user));
    let logon = if with_logon {
        format!(
            "    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>{}</UserId>\n    </LogonTrigger>\n",
            xml_escape(user)
        )
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Dead Stock Finder</Author>
    <Description>Checks Square for items that haven't sold and updates their Last sold labels.</Description>
    <URI>{name}</URI>
  </RegistrationInfo>
  <Triggers>
{logon}    <CalendarTrigger>
      <StartBoundary>2026-01-01T{h:02}:{m:02}:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT10M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>--check --trigger scheduled</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        name = xml_escape(&name),
        h = check_time.0,
        m = check_time.1,
        user = xml_escape(user),
        exe = xml_escape(exe),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Registers the background check. `check_time` matters on Windows (the
/// task's daily time); macOS ticks and the check decides.
pub fn register(check_time: (u8, u8)) -> Result<Registration, ScheduleError> {
    os::register(check_time)
}

/// Removes every background trigger this app made.
pub fn unregister() -> Result<(), ScheduleError> {
    os::unregister()
}

/// What is registered now.
pub fn status() -> Registration {
    os::status()
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;
    use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo, NSString};
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use std::process::Command;

    fn at_least_13() -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 13,
            minorVersion: 0,
            patchVersion: 0,
        })
    }

    fn agent(label: &str) -> objc2::rc::Retained<SMAppService> {
        unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(&format!("{label}.plist"))) }
    }

    fn describe(status: SMAppServiceStatus) -> &'static str {
        match status {
            SMAppServiceStatus::Enabled => "enabled",
            SMAppServiceStatus::RequiresApproval => {
                "requires approval in System Settings > General > Login Items"
            }
            SMAppServiceStatus::NotFound => "not found in the app bundle",
            _ => "not registered",
        }
    }

    fn legacy_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/LaunchAgents"))
    }

    fn uid() -> String {
        Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    pub fn register(_check_time: (u8, u8)) -> Result<Registration, ScheduleError> {
        if at_least_13() {
            let mut details = Vec::new();
            for label in [SIGNIN_LABEL, DAILY_LABEL] {
                let service = agent(label);
                if unsafe { service.status() } != SMAppServiceStatus::Enabled {
                    if let Err(e) = unsafe { service.registerAndReturnError() } {
                        let status = unsafe { service.status() };
                        if status != SMAppServiceStatus::RequiresApproval {
                            return Err(ScheduleError::Failed(format!(
                                "{label}: {}",
                                e.localizedDescription()
                            )));
                        }
                    }
                }
                details.push(format!("{label}: {}", describe(unsafe { service.status() })));
            }
            return Ok(Registration { method: Method::SmAppService, detail: details.join("; ") });
        }
        // macOS 11–12.
        let dir = legacy_dir().ok_or_else(|| ScheduleError::Failed("no home folder".into()))?;
        std::fs::create_dir_all(&dir).map_err(|e| ScheduleError::Failed(e.to_string()))?;
        let exe = std::env::current_exe().map_err(|e| ScheduleError::Failed(e.to_string()))?;
        let exe = exe.to_string_lossy();
        let uid = uid();
        for (label, trigger) in [(SIGNIN_LABEL, Trigger::Signin), (DAILY_LABEL, Trigger::Tick)] {
            let path = dir.join(format!("{label}.plist"));
            std::fs::write(&path, launch_agent_plist(label, &exe, false, trigger))
                .map_err(|e| ScheduleError::Failed(e.to_string()))?;
            let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{label}")]).output();
            let out = Command::new("launchctl")
                .args(["bootstrap", &format!("gui/{uid}"), &path.to_string_lossy()])
                .output()
                .map_err(|e| ScheduleError::Failed(e.to_string()))?;
            if !out.status.success() {
                return Err(ScheduleError::Failed(format!(
                    "launchctl bootstrap {label}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        Ok(Registration {
            method: Method::LaunchAgentsFolder,
            detail: format!("loaded from {}", dir.display()),
        })
    }

    pub fn unregister() -> Result<(), ScheduleError> {
        if at_least_13() {
            for label in [SIGNIN_LABEL, DAILY_LABEL] {
                let service = agent(label);
                if unsafe { service.status() } != SMAppServiceStatus::NotRegistered {
                    let _ = unsafe { service.unregisterAndReturnError() };
                }
            }
        }
        // Clean up the legacy files too, in case macOS was upgraded.
        if let Some(dir) = legacy_dir() {
            let uid = uid();
            for label in [SIGNIN_LABEL, DAILY_LABEL] {
                let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{label}")]).output();
                let _ = std::fs::remove_file(dir.join(format!("{label}.plist")));
            }
        }
        Ok(())
    }

    pub fn status() -> Registration {
        if at_least_13() {
            let statuses: Vec<SMAppServiceStatus> =
                [SIGNIN_LABEL, DAILY_LABEL].iter().map(|l| unsafe { agent(l).status() }).collect();
            let detail = [SIGNIN_LABEL, DAILY_LABEL]
                .iter()
                .zip(&statuses)
                .map(|(l, s)| format!("{l}: {}", describe(*s)))
                .collect::<Vec<_>>()
                .join("; ");
            let any = statuses
                .iter()
                .any(|s| *s == SMAppServiceStatus::Enabled || *s == SMAppServiceStatus::RequiresApproval);
            return Registration { method: if any { Method::SmAppService } else { Method::None }, detail };
        }
        let present = legacy_dir().is_some_and(|d| d.join(format!("{DAILY_LABEL}.plist")).exists());
        Registration {
            method: if present { Method::LaunchAgentsFolder } else { Method::None },
            detail: if present { "plists in ~/Library/LaunchAgents".into() } else { "not registered".into() },
        }
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn schtasks(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("schtasks.exe").args(args).creation_flags(CREATE_NO_WINDOW).output()
    }

    fn user() -> String {
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let name = std::env::var("USERNAME").unwrap_or_default();
        if domain.is_empty() { name } else { format!("{domain}\\{name}") }
    }

    fn name() -> String {
        task_name(&std::env::var("USERNAME").unwrap_or_default())
    }

    fn create_task(exe: &str, check_time: (u8, u8), with_logon: bool) -> Result<(), String> {
        let xml = task_xml(&user(), exe, check_time, with_logon);
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let path = std::env::temp_dir().join(format!("dead-stock-finder-task-{}.xml", std::process::id()));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let out = schtasks(&["/Create", "/TN", &name(), "/XML", &path.to_string_lossy(), "/F"]);
        let _ = std::fs::remove_file(&path);
        let out = out.map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} {}",
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn set_run_key(exe: &str) -> Result<(), String> {
        let key_path = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        // A new Windows user may not have a Run key yet.
        let ok = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key_path.as_ptr(),
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
            return Err(format!("open Run key: {ok}"));
        }
        let name = wide(RUN_VALUE);
        let value = wide(&format!("\"{exe}\" --check --trigger signin"));
        let ok = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            )
        };
        unsafe { RegCloseKey(key) };
        if ok != 0 { Err(format!("set Run value: {ok}")) } else { Ok(()) }
    }

    fn run_key_present() -> bool {
        let key_path = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_path.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            return false;
        }
        let name = wide(RUN_VALUE);
        let found = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0;
        unsafe { RegCloseKey(key) };
        found
    }

    fn delete_run_key() {
        let key_path = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_path.as_ptr(), 0, KEY_WRITE, &mut key) } == 0 {
            let name = wide(RUN_VALUE);
            unsafe {
                RegDeleteValueW(key, name.as_ptr());
                RegCloseKey(key);
            }
        }
    }

    pub fn register(check_time: (u8, u8)) -> Result<Registration, ScheduleError> {
        let exe = std::env::current_exe().map_err(|e| ScheduleError::Failed(e.to_string()))?;
        let exe = exe.to_string_lossy().to_string();
        match create_task(&exe, check_time, true) {
            Ok(()) => {
                delete_run_key();
                Ok(Registration {
                    method: Method::TaskScheduler,
                    detail: format!("task {}: sign-in and daily", name()),
                })
            }
            Err(full) => match create_task(&exe, check_time, false) {
                Ok(()) => {
                    set_run_key(&exe).map_err(|e| {
                        ScheduleError::Failed(format!("sign-in trigger: {full}; Run entry: {e}"))
                    })?;
                    Ok(Registration {
                        method: Method::TaskSchedulerDailyAndRunKey,
                        detail: format!("sign-in trigger refused ({full}); daily task plus Run entry"),
                    })
                }
                Err(daily) => {
                    set_run_key(&exe).map_err(|e| {
                        ScheduleError::Failed(format!(
                            "task with sign-in: {full}; daily task: {daily}; Run entry: {e}"
                        ))
                    })?;
                    Ok(Registration {
                        method: Method::RunKeyOnly,
                        detail: format!("task refused ({daily}); Run entry only, daily check at sign-in"),
                    })
                }
            },
        }
    }

    pub fn unregister() -> Result<(), ScheduleError> {
        let _ = schtasks(&["/Delete", "/TN", &name(), "/F"]);
        delete_run_key();
        Ok(())
    }

    pub fn status() -> Registration {
        let task = schtasks(&["/Query", "/TN", &name(), "/XML"]).ok().filter(|o| o.status.success());
        let run = run_key_present();
        match (task, run) {
            (Some(out), false) => {
                let xml = String::from_utf8_lossy(&out.stdout).to_string();
                let detail = if xml.contains("LogonTrigger") { "sign-in and daily" } else { "daily only" };
                Registration { method: Method::TaskScheduler, detail: format!("task {}: {detail}", name()) }
            }
            (Some(_), true) => Registration {
                method: Method::TaskSchedulerDailyAndRunKey,
                detail: "daily task plus Run entry".into(),
            },
            (None, true) => Registration { method: Method::RunKeyOnly, detail: "Run entry only".into() },
            (None, false) => Registration { method: Method::None, detail: "not registered".into() },
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use super::*;

    pub fn register(_check_time: (u8, u8)) -> Result<Registration, ScheduleError> {
        Err(ScheduleError::Unsupported)
    }

    pub fn unregister() -> Result<(), ScheduleError> {
        Ok(())
    }

    pub fn status() -> Registration {
        Registration { method: Method::None, detail: "background checks are macOS and Windows only".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A fixed-offset zone is enough for the due logic.
    fn ny() -> chrono::FixedOffset {
        chrono::FixedOffset::west_opt(5 * 3600).unwrap()
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn last_occurrence_today_or_yesterday() {
        let tz = ny();
        // 06:00 local (11:00 UTC): today's 05:00 has passed.
        assert_eq!(last_occurrence(&tz, utc(2026, 3, 2, 11, 0), 5, 0), utc(2026, 3, 2, 10, 0));
        // 04:00 local: yesterday's.
        assert_eq!(last_occurrence(&tz, utc(2026, 3, 2, 9, 0), 5, 0), utc(2026, 3, 1, 10, 0));
    }

    #[test]
    fn tick_runs_once_per_day() {
        let tz = ny();
        let now = utc(2026, 3, 2, 10, 15); // 05:15 local
        assert!(tick_due_in(&tz, now, (5, 0), None, None));
        // Yesterday's success: due.
        assert!(tick_due_in(&tz, now, (5, 0), Some(utc(2026, 3, 1, 10, 0)), Some(utc(2026, 3, 1, 10, 0))));
        // Already succeeded at 05:00 today: not due.
        assert!(!tick_due_in(&tz, now, (5, 0), Some(utc(2026, 3, 2, 10, 0)), Some(utc(2026, 3, 2, 10, 0))));
        // Failed at 05:00 today: retry only after two hours.
        assert!(!tick_due_in(&tz, now, (5, 0), Some(utc(2026, 3, 2, 10, 0)), Some(utc(2026, 3, 1, 10, 0))));
        assert!(tick_due_in(
            &tz,
            utc(2026, 3, 2, 12, 0),
            (5, 0),
            Some(utc(2026, 3, 2, 10, 0)),
            Some(utc(2026, 3, 1, 10, 0))
        ));
        // Before today's time, after yesterday's success: not due.
        assert!(!tick_due_in(
            &tz,
            utc(2026, 3, 2, 9, 0),
            (5, 0),
            Some(utc(2026, 3, 1, 10, 0)),
            Some(utc(2026, 3, 1, 10, 0))
        ));
        // A sign-in check at 04:50 doesn't count for today's 05:00.
        assert!(tick_due_in(&tz, now, (5, 0), Some(utc(2026, 3, 2, 9, 50)), Some(utc(2026, 3, 2, 9, 50))));
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let first = try_lock(dir.path()).unwrap();
        assert!(first.is_some());
        assert!(try_lock(dir.path()).unwrap().is_none());
        drop(first);
        assert!(try_lock(dir.path()).unwrap().is_some());
    }

    #[test]
    fn plists_carry_the_right_arguments() {
        let tick = launch_agent_plist(DAILY_LABEL, BUNDLE_PROGRAM, true, Trigger::Tick);
        assert!(tick.contains("<key>BundleProgram</key>"));
        assert!(tick.contains("<string>tick</string>"));
        assert!(tick.contains("<key>StartCalendarInterval</key>"));
        assert!(tick.contains("<key>RunAtLoad</key>\n\t<false/>"));
        assert_eq!(tick.matches("<key>Minute</key>").count(), 4);
        let signin = launch_agent_plist(
            SIGNIN_LABEL,
            "/Applications/X.app/Contents/MacOS/dead-stock-finder",
            false,
            Trigger::Signin,
        );
        assert!(signin.contains("<key>Program</key>"));
        assert!(signin.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(!signin.contains("StartCalendarInterval"));
    }

    /// The plists shipped in the app bundle are exactly what this code
    /// generates (tools/gen-plists regenerates them).
    #[test]
    fn bundled_plists_match() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src-tauri/macos/LaunchAgents");
        for (label, trigger) in [(SIGNIN_LABEL, Trigger::Signin), (DAILY_LABEL, Trigger::Tick)] {
            let path = dir.join(format!("{label}.plist"));
            let expected = launch_agent_plist(label, BUNDLE_PROGRAM, true, trigger);
            match std::fs::read_to_string(&path) {
                Ok(actual) => assert_eq!(actual, expected, "{} is out of date", path.display()),
                Err(_) => {
                    if std::env::var_os("DSF_WRITE_PLISTS").is_some() {
                        std::fs::create_dir_all(&dir).unwrap();
                        std::fs::write(&path, expected).unwrap();
                    } else {
                        panic!("{} missing (run with DSF_WRITE_PLISTS=1 to create)", path.display());
                    }
                }
            }
        }
    }

    #[test]
    fn task_xml_has_both_triggers_and_no_admin() {
        let xml = task_xml(
            "PC\\owner",
            "C:\\Users\\owner\\AppData\\Local\\Dead Stock Finder\\dead-stock-finder.exe",
            (5, 30),
            true,
        );
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<StartBoundary>2026-01-01T05:30:00</StartBoundary>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT10M</ExecutionTimeLimit>"));
        assert!(xml.contains("<URI>\\Dead Stock Finder check (owner)</URI>"));
        assert!(!task_xml("u", "e", (5, 0), false).contains("LogonTrigger"));
        assert_eq!(task_name("a/b:c"), "\\Dead Stock Finder check (abc)");
    }
}
