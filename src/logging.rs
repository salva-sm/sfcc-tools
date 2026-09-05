use chrono::Local;

pub fn stamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

pub fn info(message: impl AsRef<str>) {
    crate::out!("[{}] {}", stamp(), message.as_ref());
}

pub fn ok(message: impl AsRef<str>) {
    crate::out!("[{}] OK  {}", stamp(), message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    crate::out!("[{}] !   {}", stamp(), message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    crate::errout!("[{}] ERR {}", stamp(), message.as_ref());
}

pub fn upload(rel_path: &str) {
    crate::out!("[{}] ->  {}", stamp(), rel_path);
}

pub fn removal(rel_path: &str) {
    crate::out!("[{}] x   {}", stamp(), rel_path);
}

#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! outp {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! errout {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}
