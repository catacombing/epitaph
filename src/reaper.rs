//! Watchdog for spawning subprocesses.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};

use calloop::signals::{Signal, Signals};
use calloop::LoopHandle;

use crate::{Result, State};

/// Callback invoked after reaping.
type Callback = Box<dyn FnOnce(&mut State, Output)>;

/// Watchdog for reaping dead children.
pub struct Reaper {
    processes: HashMap<u32, (Child, Callback)>,
}

impl Reaper {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Register calloop SIGCHLD handler.
        let signals = Signals::new(&[Signal::SIGCHLD]).unwrap();
        event_loop.insert_source(signals, |signal, _, state| {
            if let Ok((callback, output)) = state.reaper.kill(signal.full_info().ssi_pid) {
                callback(state, output);
            }
        })?;

        Ok(Self { processes: Default::default() })
    }

    /// Start watching a child.
    pub fn watch(&mut self, mut child: Command, callback: Callback) {
        // Set STDIO handles so callees don't have to handle it.
        child.stdin(Stdio::null());
        child.stdout(Stdio::piped());
        child.stderr(Stdio::piped());

        // Try to spawn the child process.
        let child = match child.spawn() {
            Ok(child) => child,
            Err(err) => {
                println!("Error: Child process failed: {}", err);
                return;
            },
        };

        let pid = child.id();
        self.processes.insert(pid, (child, callback));
    }

    /// Kill a kid.
    pub fn kill(&mut self, pid: u32) -> Result<(Callback, Output)> {
        let (mut child, callback) = match self.processes.remove(&pid) {
            Some(process) => process,
            None => return Err(format!("{pid}: PID not supervised by reaper").into()),
        };

        // Ensure child is dead.
        let _ = child.kill();

        // Wait for child completion.
        let output = child.wait_with_output()?;

        Ok((callback, output))
    }
}

/// Spawn unsupervised daemons.
///
/// This will double-fork to avoid spawning zombies, but does not provide any
/// ability to retrieve the process output.
pub fn daemon<I, S>(program: S, args: I) -> io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());

    unsafe {
        command.pre_exec(|| {
            match libc::fork() {
                -1 => return Err(io::Error::last_os_error()),
                0 => (),
                _ => libc::_exit(0),
            }

            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }

            Ok(())
        });
    }

    command.spawn()?.wait()?;

    Ok(())
}
