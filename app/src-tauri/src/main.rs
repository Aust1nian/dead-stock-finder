// Release builds on Windows have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod config;
mod headless;
mod window;

use cli::Mode;

fn main() {
    let args = cli::parse(std::env::args().skip(1));
    if args.mode != Mode::Window {
        attach_parent_console();
    }
    let code = match args.mode {
        Mode::Window => {
            window::run();
            0
        }
        Mode::Check(trigger) => headless::check(trigger),
        Mode::SelfTest => headless::self_test(args.json_out.as_deref()),
        Mode::Schedule(action) => headless::schedule(&action, args.json_out.as_deref()),
        Mode::Uninstall { remove_data } => headless::uninstall(remove_data),
        Mode::HoldLock(seconds) => headless::hold_lock(seconds),
        Mode::MarkSetup => headless::mark_setup(),
    };
    std::process::exit(code);
}

/// A release Windows build has no console; when started from one (a
/// developer or CI running `--self-test`), print there. A scheduled check
/// has no parent console and stays invisible.
fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}
