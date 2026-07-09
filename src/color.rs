//! Lightweight terminal color helpers for CLI output.
//!
//! Respects `NO_COLOR` and only emits ANSI codes when stderr/stdout is a TTY,
//! so piping into `less`/`head` stays clean.

use std::io::IsTerminal;
use std::sync::OnceLock;

static COLOR_ENABLED: OnceLock<bool> = OnceLock::new();

fn enabled() -> bool {
    *COLOR_ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if std::env::var_os("FORCE_COLOR").is_some() {
            return true;
        }
        std::io::stdout().is_terminal()
    })
}

fn paint(code: &str, text: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn green(text: &str) -> String {
    paint("32", text)
}

pub fn red(text: &str) -> String {
    paint("31", text)
}

pub fn yellow(text: &str) -> String {
    paint("33", text)
}

pub fn cyan(text: &str) -> String {
    paint("36", text)
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn success(msg: &str) {
    println!("{} {msg}", green("✓"));
}

pub fn error(msg: &str) {
    eprintln!("{} {msg}", red("✗"));
}

pub fn warn(msg: &str) {
    eprintln!("{} {msg}", yellow("!"));
}

pub fn info(msg: &str) {
    println!("{} {msg}", cyan("•"));
}
