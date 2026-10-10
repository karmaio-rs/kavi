use std::{future::poll_fn, task::Poll};

/// Bounds consecutive ready iterations on a Karmaio worker.
pub(crate) struct WorkBudget(u8);

impl WorkBudget {
    pub(crate) const fn new() -> Self {
        Self(16)
    }

    pub(crate) async fn step(&mut self) {
        if self.0 == 0 {
            let mut yielded = false;
            poll_fn(|context| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            self.0 = 16;
        }
        self.0 -= 1;
    }
}
