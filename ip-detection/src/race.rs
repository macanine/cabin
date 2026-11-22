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
) -> Vec<T>
where
    S: Send + 'static,
    T: Send + 'static,
{
    let mut sources = sources.into_iter().peekable();
    let (send, receive) = mpsc::channel();
    let query = Arc::new(query);
    let deadline = Instant::now() + schedule.deadline;
    let start = |source: S| {
        let send = send.clone();
        let query = Arc::clone(&query);
        let timeout = schedule
            .request_timeout
            .min(deadline.saturating_duration_since(Instant::now()));
        std::thread::spawn(move || {
            let _ = send.send(query(source, timeout.max(Duration::from_millis(1))));
        });
    };
    let mut in_flight = 0;
    for source in sources.by_ref().take(schedule.initial) {
        start(source);
        in_flight += 1;
    }
    let mut next_hedge = Instant::now() + schedule.hedge;
    let mut outcomes = Vec::new();
    while in_flight > 0 {
        let may_start = in_flight < schedule.parallel && sources.peek().is_some();
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let next_event = if may_start {
            deadline.min(next_hedge)
        } else {
            deadline
        };
        match receive.recv_timeout(next_event.saturating_duration_since(now)) {
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
                    next_hedge = Instant::now() + schedule.hedge;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline && may_start => {
                start(sources.next().expect("pending source"));
                in_flight += 1;
                next_hedge = Instant::now() + schedule.hedge;
            }
            Err(_) => break,
        }
    }
    outcomes
}
