//! Command-line modes. No flag opens the window; the rest run headless.

use std::path::PathBuf;

use dsf_schedule::Trigger;

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Window,
    /// `--check [--trigger signin|scheduled|tick|manual]`: the headless check.
    Check(Trigger),
    /// `--self-test`: a full check against bundled mock data; exits 0 or 1.
    SelfTest,
    /// `--schedule register|unregister|status|restore`.
    Schedule(String),
    /// `--uninstall [--remove-data]`: run by the Windows uninstaller.
    Uninstall {
        remove_data: bool,
    },
    /// `--hold-lock <seconds>`: holds the check lock (scheduler tests).
    HoldLock(u64),
    /// `--test-mark-setup`: marks setup done without connecting, so the
    /// scheduler tests can watch a background check run (it ends "not
    /// connected"). Changes nothing else.
    MarkSetup,
}

#[derive(Debug)]
pub struct Args {
    pub mode: Mode,
    /// Also write the result as JSON here (Windows release builds have no
    /// console to print to).
    pub json_out: Option<PathBuf>,
}

pub fn parse(args: impl Iterator<Item = String>) -> Args {
    let args: Vec<String> = args.collect();
    let value_after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let json_out = value_after("--json-out").map(PathBuf::from);
    let mode = if has("--check") {
        Mode::Check(value_after("--trigger").and_then(|t| Trigger::parse(&t)).unwrap_or(Trigger::Scheduled))
    } else if has("--self-test") {
        Mode::SelfTest
    } else if let Some(action) = value_after("--schedule") {
        Mode::Schedule(action)
    } else if has("--uninstall") {
        Mode::Uninstall { remove_data: has("--remove-data") }
    } else if let Some(secs) = value_after("--hold-lock") {
        Mode::HoldLock(secs.parse().unwrap_or(30))
    } else if has("--test-mark-setup") {
        Mode::MarkSetup
    } else {
        Mode::Window
    };
    Args { mode, json_out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Args {
        parse(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn modes() {
        assert_eq!(p("").mode, Mode::Window);
        assert_eq!(p("--check").mode, Mode::Check(Trigger::Scheduled));
        assert_eq!(p("--check --trigger tick").mode, Mode::Check(Trigger::Tick));
        assert_eq!(p("--check --trigger signin").mode, Mode::Check(Trigger::Signin));
        assert_eq!(p("--self-test --json-out x.json").mode, Mode::SelfTest);
        assert_eq!(p("--self-test --json-out x.json").json_out, Some(PathBuf::from("x.json")));
        assert_eq!(p("--schedule status").mode, Mode::Schedule("status".into()));
        assert_eq!(p("--uninstall --remove-data").mode, Mode::Uninstall { remove_data: true });
        // macOS adds -psn_… when launched from Finder on old systems.
        assert_eq!(p("-psn_0_12345").mode, Mode::Window);
    }
}
