//! Recoverable editor sessions: errors never discard the latest buffer.
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("edit cancelled")
    }
}
impl std::error::Error for Cancelled {}

pub(crate) fn launch(path: &Path) -> io::Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let args = shell_words::split(&editor).map_err(io::Error::other)?;
    let (program, args) = args
        .split_first()
        .ok_or_else(|| io::Error::other("empty editor command"))?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("editor failed: {status}")))
    }
}

/// Read the controlling terminal, never a redirected stdin payload.
#[cfg(unix)]
fn key() -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let fd = tty.as_raw_fd();
    // SAFETY: termios points to valid storage and fd remains open for this scope.
    let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return Err(io::Error::last_os_error());
    }
    struct Restore(i32, libc::termios);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: fd remains owned by tty, declared before this guard.
            unsafe {
                libc::tcsetattr(self.0, libc::TCSANOW, &self.1);
            }
        }
    }
    let _restore = Restore(fd, original);
    let mut raw = original;
    // SAFETY: valid termios and live fd.
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    eprint!("Press any key to edit again, Ctrl-C to exit");
    io::stderr().flush()?;
    let mut byte = [0u8];
    tty.read_exact(&mut byte)?;
    if byte[0] == 27 || byte[0] >= 128 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // SAFETY: flushing only pending controlling-terminal input.
    if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
        return Err(io::Error::last_os_error());
    }
    eprintln!();
    Ok(byte[0] != 3)
}

#[cfg(not(unix))]
fn key() -> io::Result<bool> {
    use crossterm::event::{Event, KeyCode, KeyModifiers, read};
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    crossterm::terminal::enable_raw_mode()?;
    let _restore = Restore;
    eprint!("Press any key to edit again, Ctrl-C to exit");
    io::stderr().flush()?;
    loop {
        if let Event::Key(key) = read()? {
            return Ok(
                !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            );
        }
    }
}

/// On cancellation/fallback retain the same private file for manual recovery.
pub(crate) fn retry(
    file: &mut Option<tempfile::NamedTempFile>,
    error: &dyn std::fmt::Display,
    raw: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if io::stderr().is_terminal() && !raw {
        eprintln!("\x1b[1;31m{error}\x1b[0m");
    } else {
        eprintln!("{error}");
    }
    match key() {
        Ok(true) => Ok(()),
        outcome => {
            if let Some(file) = file.take() {
                let (_, path) = file.keep()?;
                eprintln!("Edits preserved: {}", path.display());
            }
            match outcome {
                Ok(false) => Err(Box::new(Cancelled)),
                Err(e) => Err(format!("cannot retry interactively: {e}").into()),
                Ok(true) => unreachable!(),
            }
        }
    }
}
