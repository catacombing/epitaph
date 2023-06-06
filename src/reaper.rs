//! Watchdog for spawning subprocesses.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{self, Read};
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
        event_loop.insert_source(signals, |_, _, state| {
            // Find all dead children.
            let mut zombies = Vec::new();
            for (pid, (child, _)) in &mut state.reaper.processes {
                if let Some(output) = Self::try_reap(child) {
                    zombies.push((*pid, output));
                }
            }

            // Remove dead children and call their callbacks.
            for (pid, output) in zombies.drain(..) {
                if let Some((_, callback)) = state.reaper.processes.remove(&pid) {
                    callback(state, output);
                }
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
                eprintln!("Error: Child process failed: {err}");
                return;
            },
        };

        let pid = child.id();
        self.processes.insert(pid, (child, callback));
    }

    /// Try and reap a child.
    pub fn try_reap(child: &mut Child) -> Option<Output> {
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            // Skip reaping if child is not dead.
            Ok(None) | Err(_) => return None,
        };

        // Read STDOUT to buffer.
        let mut stdout = Vec::new();
        if let Some(mut child_stdout) = child.stdout.take() {
            let _ = child_stdout.read_to_end(&mut stdout);
        }

        // Read STDERR to buffer.
        let mut stderr = Vec::new();
        if let Some(mut child_stderr) = child.stderr.take() {
            let _ = child_stderr.read_to_end(&mut stderr);
        }

        Some(Output { status, stdout, stderr })
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
