use std::io;
#[cfg(unix)]
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};

/// An explicitly installed process shutdown trigger.
///
/// Construction registers Ctrl-C and, on Unix, SIGTERM before serving begins.
/// No process is exited and no application resources are closed. Applications
/// can instead pass their own fallible stop future to [`super::Serve::run_until`].
pub struct ShutdownSignal {
    interrupt: karmaio::signal::CtrlC,
    #[cfg(unix)]
    terminate: karmaio::signal::Signal,
}

impl ShutdownSignal {
    /// Registers the process notifications used to initiate shutdown.
    ///
    /// # Errors
    /// Returns an error if an operating-system signal listener cannot be installed.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: karmaio::signal::ctrl_c()?,
            #[cfg(unix)]
            terminate: karmaio::signal::signal(karmaio::signal::SignalKind::terminate())?,
        })
    }

    /// Waits for the first registered notification.
    ///
    /// # Errors
    /// Returns operating-system signal observation errors to the serving caller.
    pub async fn wait(self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mut terminate = self.terminate;
            let mut interrupt = pin!(self.interrupt);
            let mut terminate = pin!(terminate.recv());
            poll_fn(|cx| {
                if let Poll::Ready(result) = interrupt.as_mut().poll(cx) {
                    return Poll::Ready(result);
                }
                terminate.as_mut().poll(cx)
            })
            .await
        }
        #[cfg(not(unix))]
        self.interrupt.await
    }
}
