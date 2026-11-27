use std::io;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub(super) struct Schedule {
    deadline: Duration,
    request_timeout: Duration,
    hedge: Duration,
    initial: usize,
    parallel: usize,
    refill_on_result: bool,
}

impl Schedule {
    pub fn address(timeout: Duration) -> Self {
        Self {
            deadline: timeout,
            request_timeout: timeout,
            hedge: Duration::from_millis(800),
            initial: 1,
            parallel: 3,
            refill_on_result: true,
        }
    }

    pub fn quality() -> Self {
        Self {
            deadline: Duration::from_secs(11),
            request_timeout: Duration::from_secs(8),
            hedge: Duration::from_secs(1),
            initial: 3,
            parallel: 6,
            refill_on_result: false,
        }
    }
}

/// Return as soon as an answer is accepted. A blocking request already on
/// the wire can finish in the background within its own timeout.
pub(super) fn race<S, T>(
    sources: impl IntoIterator<Item = S>,
    schedule: Schedule,
    query: impl Fn(S, Duration) -> T + Send + Sync + 'static,
    accept: impl Fn(&T) -> bool,
    spawn_failed: impl Fn(S, io::Error) -> T,
) -> Vec<T>
where
    S: Clone + Send + 'static,
    T: Send + 'static,
{
    let mut sources = sources.into_iter().peekable();
    let (send, receive) = mpsc::channel();
    let query = Arc::new(query);
    let started = Instant::now();
    let start = |source: S| {
        let worker_send = send.clone();
        let worker_source = source.clone();
        let query = Arc::clone(&query);
        let Schedule {
            deadline,
            request_timeout,
            ..
        } = schedule;
        if let Err(error) = std::thread::Builder::new().spawn(move || {
            let timeout = request_timeout.min(deadline.saturating_sub(started.elapsed()));
            let _ = worker_send.send(query(worker_source, timeout));
        }) {
            let _ = send.send(spawn_failed(source, error));
        }
    };
    let mut in_flight = 0;
    for source in sources.by_ref().take(schedule.initial) {
        start(source);
        in_flight += 1;
    }
    let mut next_hedge = started.elapsed().saturating_add(schedule.hedge);
    let mut outcomes = Vec::new();
    while in_flight > 0 {
        let may_start = in_flight < schedule.parallel && sources.peek().is_some();
        let elapsed = started.elapsed();
        if elapsed >= schedule.deadline {
            break;
        }
        let next_event = if may_start {
            schedule.deadline.min(next_hedge)
        } else {
            schedule.deadline
        };
        match receive.recv_timeout(next_event.saturating_sub(elapsed)) {
            Ok(answer) => {
                in_flight -= 1;
                let accepted = accept(&answer);
                outcomes.push(answer);
                if accepted {
                    break;
                }
                if (in_flight == 0 || schedule.refill_on_result)
                    && let Some(source) = sources.next()
                {
                    start(source);
                    in_flight += 1;
                    next_hedge = started.elapsed().saturating_add(schedule.hedge);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout)
                if started.elapsed() < schedule.deadline
                    && may_start
                    && let Some(source) = sources.next() =>
            {
                start(source);
                in_flight += 1;
                next_hedge = started.elapsed().saturating_add(schedule.hedge);
            }
            Err(_) => break,
        }
    }
    outcomes
}
