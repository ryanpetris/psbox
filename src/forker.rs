//! A dedicated thread that forks into the sandbox's PID namespace.
//!
//! A thread that has joined another PID namespace can still fork, but can no
//! longer create threads, and a process that unshares its own PID namespace
//! cannot create threads at all. So one thread joins the sandbox's PID
//! namespace once and runs every spawn; all other threads stay outside it.

use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::mpsc;

type Job = Box<dyn FnOnce() + Send>;

pub struct Forker {
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl Forker {
    /// Starts the thread and joins it to the PID namespace of `init_pidfd`.
    pub fn start(init_pidfd: RawFd) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<io::Result<()>>(1);
        std::thread::Builder::new().name("forker".into()).spawn(move || {
            // SAFETY: setns on this thread only; the pidfd stays open for the
            // supervisor's lifetime.
            if unsafe { libc::setns(init_pidfd, libc::CLONE_NEWPID) } < 0 {
                let _ = ready_tx.send(Err(io::Error::last_os_error()));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            for job in rx {
                job();
            }
        })?;
        ready_rx
            .recv()
            .map_err(|_| io::Error::other("forker thread exited"))??;
        Ok(Self { jobs: Mutex::new(tx) })
    }

    /// Runs `f` on the forker thread and returns its result.
    pub fn run<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
        let (tx, rx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let _ = tx.send(f());
        });
        self.jobs
            .lock()
            .unwrap()
            .send(job)
            .map_err(|_| io::Error::other("forker thread exited"))?;
        rx.recv().map_err(|_| io::Error::other("forker job panicked"))
    }
}
