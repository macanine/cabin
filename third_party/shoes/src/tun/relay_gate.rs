use std::future::{Future, poll_fn};
use std::task::Poll;
use tokio::sync::watch;

/// A pause barrier shared by the host and the TUN stack. A suspension waits
/// for current I/O polls to finish. Later polls wait for the host to resume.
#[derive(Clone)]
pub struct RelayGate(watch::Sender<RelayState>);

/// Permission for an operation that must be cancelled at the next suspension.
#[derive(Clone)]
pub struct RelayTicket {
    gate: RelayGate,
    epoch: u64,
}

impl RelayTicket {
    pub fn is_current(&self) -> bool {
        let state = self.gate.state();
        state.open && state.epoch == self.epoch
    }

    pub async fn run<F: Future>(&self, future: F) -> Option<F::Output> {
        self.gate.run(|current| current == self.epoch, future).await
    }
}

#[derive(Clone, Copy)]
pub(super) struct RelayState {
    pub open: bool,
    pub epoch: u64,
}

impl Default for RelayGate {
    fn default() -> Self {
        Self(watch::Sender::new(RelayState {
            open: true,
            epoch: 0,
        }))
    }
}

impl RelayGate {
    /// Suspend the TUN path and clear its host-owned packet queues while
    /// neither the TUN device nor its relay tasks can access them.
    pub fn suspend(&self, clear_queues: impl FnOnce()) {
        self.0.send_if_modified(|state| {
            if !state.open {
                return false;
            }
            state.open = false;
            state.epoch += 1;
            clear_queues();
            true
        });
    }

    pub fn resume(&self) {
        self.0.send_modify(|state| state.open = true);
    }

    pub fn ticket(&self) -> Option<RelayTicket> {
        let state = self.state();
        state.open.then(|| RelayTicket {
            gate: self.clone(),
            epoch: state.epoch,
        })
    }

    pub(super) fn state(&self) -> watch::Ref<'_, RelayState> {
        self.0.borrow()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<RelayState> {
        self.0.subscribe()
    }

    /// Poll I/O only while open. A generation-sensitive operation drops its
    /// future whenever `eligible` rejects the current epoch, even if a pause
    /// and resume coalesce into a single watch notification.
    pub(super) async fn run<F: Future>(
        &self,
        eligible: impl Fn(u64) -> bool,
        future: F,
    ) -> Option<F::Output> {
        let mut updates = self.subscribe();
        let mut future = std::pin::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = updates.changed() => {}
                result = poll_fn(|cx| {
                    let state = self.state();
                    if !eligible(state.epoch) {
                        return Poll::Ready(None);
                    }
                    if state.open {
                        future.as_mut().poll(cx).map(Some)
                    } else {
                        Poll::Pending
                    }
                }) => return result,
            }
        }
    }
}
