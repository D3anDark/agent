use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// Every child runs in its own session with stdin closed, so it cannot open `/dev/tty` or read the TUI's keys:
/// `zsh -i`, `script` or a password prompt would otherwise take the terminal's foreground group and stop the TUI.
/// The session is also a process group, so `kill -- -pid` reaches every child.
#[allow(clippy::disallowed_methods)]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    // SAFETY: the hook only calls setsid, which is async-signal-safe.
    unsafe { command.pre_exec(new_session) };
    command
}

fn new_session() -> std::io::Result<()> {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    // SAFETY: setsid takes no arguments; it fails only for a process group leader, which a freshly forked child is not.
    if unsafe { setsid() } == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

static HANGUP_PIPE: AtomicI32 = AtomicI32::new(-1);

/// Runs `then` on its own thread once the terminal hangs up. Children from `command` sit in their own sessions,
/// so the kernel's SIGHUP no longer reaches them and `then` must end them.
pub fn on_hangup(then: impl FnOnce() + Send + 'static) {
    const SIGHUP: i32 = 1;
    unsafe extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    extern "C" fn wake(_: i32) {
        // SAFETY: write is async-signal-safe and the socket is never closed.
        unsafe { write(HANGUP_PIPE.load(Ordering::Relaxed), [1u8].as_ptr().cast(), 1) };
    }
    let (sender, mut receiver) = UnixStream::pair().expect("socketpair for SIGHUP");
    HANGUP_PIPE.store(sender.into_raw_fd(), Ordering::Relaxed);
    // SAFETY: the handler only calls write. exec resets a caught signal to its default, so children still die on SIGHUP.
    unsafe { signal(SIGHUP, wake) };
    std::thread::spawn(move || {
        if receiver.read(&mut [0]).is_ok_and(|n| n == 1) {
            then();
        }
    });
}

/// Sends SIGHUP to the process group of every direct child, each a session leader from `command`.
pub fn hang_up_children() {
    let Ok(output) = command("pgrep").arg("-P").arg(std::process::id().to_string()).output() else { return };
    for pid in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        let _ = command("kill").args(["-HUP", "--", &format!("-{pid}")]).status();
    }
}
