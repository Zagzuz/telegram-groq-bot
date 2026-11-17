mod context;
mod processor;

use std::{pin::Pin, sync::Arc, time::Duration};

use tokio::sync::{Notify, watch};

pub use processor::Processor;

/// In-process signal from the webhook to the job worker.
///
/// `notify_one` stores a single permit when the worker is busy. That is enough:
/// after each job the worker claims again until the queue is empty.
#[derive(Clone, Debug)]
pub struct WorkerWake {
    notify: Arc<Notify>,
}

impl WorkerWake {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn wake(&self) {
        self.notify.notify_one();
    }
}

const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const NEON_SUSPEND_AFTER: Duration = Duration::from_secs(5 * 60);

struct WorkerTiming {
    /// Delay after a failed claim or schedule read. `WORKER_POLL_MILLISECONDS`.
    error_backoff: Duration,
    /// Longest sleep when nothing is queued. `WORKER_IDLE_TIMEOUT_SECONDS`.
    idle_timeout: Duration,
    cleanup_interval: Duration,
}

pub async fn run(processor: Processor, wake: WorkerWake, shutdown: watch::Receiver<bool>) {
    let timing = WorkerTiming {
        error_backoff: processor.poll_interval(),
        idle_timeout: processor.idle_timeout(),
        cleanup_interval: CLEANUP_INTERVAL,
    };
    if timing.idle_timeout < NEON_SUSPEND_AFTER {
        tracing::warn!(
            idle_timeout_seconds = timing.idle_timeout.as_secs(),
            "WORKER_IDLE_TIMEOUT_SECONDS is under 5 minutes; Neon may not suspend between queue checks"
        );
    }
    run_loop(LiveWorker(processor), wake, shutdown, timing).await;
    tracing::info!("job worker stopped");
}

trait WorkerCycle {
    async fn process_next(&self) -> anyhow::Result<bool>;
    async fn time_until_next_job(&self) -> anyhow::Result<Option<Duration>>;
    async fn cleanup_stored_rows(&self) -> anyhow::Result<()>;
}

struct LiveWorker(Processor);

impl WorkerCycle for LiveWorker {
    async fn process_next(&self) -> anyhow::Result<bool> {
        self.0.process_next().await
    }

    async fn time_until_next_job(&self) -> anyhow::Result<Option<Duration>> {
        self.0.next_resume_in().await
    }

    async fn cleanup_stored_rows(&self) -> anyhow::Result<()> {
        self.0.cleanup().await
    }
}

/// How long an idle worker should sleep. A scheduled resume sooner than the
/// fallback wins, so retries and expired leases are not delayed by the idle
/// timeout. `None` means the queue has no future work.
fn idle_wait(next_job_in: Option<Duration>, fallback: Duration) -> Duration {
    next_job_in.map_or(fallback, |next| next.min(fallback))
}

async fn run_loop<W: WorkerCycle>(
    worker: W,
    wake: WorkerWake,
    mut shutdown: watch::Receiver<bool>,
    timing: WorkerTiming,
) {
    // `None` forces cleanup on the first iteration. Later cleanups run only
    // after the interval, and only because the worker is already awake for a
    // job, a scheduled resume, or the idle fallback. There is no cleanup timer.
    let mut last_cleanup: Option<tokio::time::Instant> = None;

    loop {
        if *shutdown.borrow() {
            break;
        }

        if last_cleanup.is_none_or(|cleaned_at| cleaned_at.elapsed() >= timing.cleanup_interval) {
            if let Err(error) = worker.cleanup_stored_rows().await {
                tracing::warn!(%error, "persistence cleanup failed");
            }
            last_cleanup = Some(tokio::time::Instant::now());
        }

        // Arm the waiter before touching the queue so an enqueue that commits
        // during the claim is not lost.
        let notified = wake.notify.notified();
        tokio::pin!(notified);
        let already_notified = notified.as_mut().enable();

        let processed = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            result = worker.process_next() => result,
        };

        match processed {
            Ok(true) => {}
            Ok(false) => {
                let keep_running = pause_for_work(
                    &worker,
                    notified.as_mut(),
                    already_notified,
                    &mut shutdown,
                    &timing,
                )
                .await;
                if !keep_running {
                    break;
                }
            }
            Err(error) => {
                tracing::error!(%error, "job worker iteration failed");
                let keep_running = if already_notified || notified_now(notified.as_mut()).await {
                    true
                } else {
                    sleep_unless_woken(notified.as_mut(), &mut shutdown, timing.error_backoff).await
                };
                if !keep_running {
                    break;
                }
            }
        }
    }
}

async fn pause_for_work<W: WorkerCycle>(
    worker: &W,
    mut notified: Pin<&mut tokio::sync::futures::Notified<'_>>,
    already_notified: bool,
    shutdown: &mut watch::Receiver<bool>,
    timing: &WorkerTiming,
) -> bool {
    if already_notified || notified_now(notified.as_mut()).await {
        return true;
    }

    let next_job_in = match worker.time_until_next_job().await {
        Ok(delay) => delay,
        Err(error) => {
            tracing::error!(%error, "failed to read the next scheduled job");
            return sleep_unless_woken(notified.as_mut(), shutdown, timing.error_backoff).await;
        }
    };
    let wait = idle_wait(next_job_in, timing.idle_timeout);
    if wait.is_zero() {
        return true;
    }

    if next_job_in.is_some() {
        tracing::debug!(
            wait_seconds = wait.as_secs(),
            "worker waiting for a scheduled retry or lease expiry"
        );
    } else {
        tracing::info!(wait_seconds = wait.as_secs(), "worker idle");
    }
    sleep_unless_woken(notified, shutdown, wait).await
}

async fn notified_now(notified: Pin<&mut tokio::sync::futures::Notified<'_>>) -> bool {
    tokio::select! {
        biased;
        _ = notified => true,
        _ = std::future::ready(()) => false,
    }
}

/// `true` means the caller should keep looping. `false` means shut down.
async fn sleep_unless_woken(
    notified: Pin<&mut tokio::sync::futures::Notified<'_>>,
    shutdown: &mut watch::Receiver<bool>,
    delay: Duration,
) -> bool {
    tokio::select! {
        biased;
        changed = shutdown.changed() => changed.is_ok() && !*shutdown.borrow(),
        _ = notified => true,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;

    #[test]
    fn idle_wait_uses_fallback_when_nothing_is_scheduled() {
        let fallback = Duration::from_secs(900);
        assert_eq!(idle_wait(None, fallback), fallback);
    }

    #[test]
    fn idle_wait_prefers_an_earlier_scheduled_resume() {
        let fallback = Duration::from_secs(900);
        assert_eq!(
            idle_wait(Some(Duration::from_secs(30)), fallback),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn idle_wait_caps_a_later_resume_at_the_fallback() {
        let fallback = Duration::from_secs(900);
        assert_eq!(
            idle_wait(Some(Duration::from_secs(10_000)), fallback),
            fallback
        );
    }

    #[test]
    fn idle_wait_is_immediate_when_the_next_job_is_due() {
        assert_eq!(
            idle_wait(Some(Duration::ZERO), Duration::from_secs(900)),
            Duration::ZERO
        );
    }

    #[derive(Clone)]
    struct ScriptedWorker {
        wake: WorkerWake,
        counts: Arc<Counts>,
        inner: Arc<Mutex<Script>>,
    }

    struct Counts {
        started: AtomicUsize,
        finished: AtomicUsize,
        resume: AtomicUsize,
        cleanup: AtomicUsize,
    }

    struct Script {
        process_results: VecDeque<Result<bool, String>>,
        resume_results: VecDeque<Result<Option<Duration>, String>>,
        notify_during_process_call: Option<usize>,
    }

    impl ScriptedWorker {
        fn new(wake: WorkerWake) -> Self {
            Self {
                wake,
                counts: Arc::new(Counts {
                    started: AtomicUsize::new(0),
                    finished: AtomicUsize::new(0),
                    resume: AtomicUsize::new(0),
                    cleanup: AtomicUsize::new(0),
                }),
                inner: Arc::new(Mutex::new(Script {
                    process_results: VecDeque::new(),
                    resume_results: VecDeque::new(),
                    notify_during_process_call: None,
                })),
            }
        }

        fn push_process(&self, result: Result<bool, &'static str>) {
            self.inner
                .lock()
                .unwrap()
                .process_results
                .push_back(result.map_err(|error| (*error).to_owned()));
        }

        fn push_resume(&self, result: Result<Option<Duration>, &'static str>) {
            self.inner
                .lock()
                .unwrap()
                .resume_results
                .push_back(result.map_err(|error| (*error).to_owned()));
        }

        fn notify_during_call(&self, call: usize) {
            self.inner.lock().unwrap().notify_during_process_call = Some(call);
        }

        fn finished(&self) -> usize {
            self.counts.finished.load(Ordering::SeqCst)
        }

        fn resume_calls(&self) -> usize {
            self.counts.resume.load(Ordering::SeqCst)
        }

        fn cleanup_calls(&self) -> usize {
            self.counts.cleanup.load(Ordering::SeqCst)
        }
    }

    impl WorkerCycle for ScriptedWorker {
        async fn process_next(&self) -> anyhow::Result<bool> {
            let call = self.counts.started.fetch_add(1, Ordering::SeqCst) + 1;
            let notify = self.inner.lock().unwrap().notify_during_process_call == Some(call);
            if notify {
                self.wake.wake();
            }
            let result = self.inner.lock().unwrap().process_results.pop_front();
            self.counts.finished.fetch_add(1, Ordering::SeqCst);
            match result {
                Some(Ok(processed)) => Ok(processed),
                Some(Err(message)) => Err(anyhow::anyhow!(message)),
                None => Ok(false),
            }
        }

        async fn time_until_next_job(&self) -> anyhow::Result<Option<Duration>> {
            self.counts.resume.fetch_add(1, Ordering::SeqCst);
            let result = self.inner.lock().unwrap().resume_results.pop_front();
            match result {
                Some(Ok(delay)) => Ok(delay),
                Some(Err(message)) => Err(anyhow::anyhow!(message)),
                None => Ok(None),
            }
        }

        async fn cleanup_stored_rows(&self) -> anyhow::Result<()> {
            self.counts.cleanup.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn timing(idle_timeout: Duration, error_backoff: Duration) -> WorkerTiming {
        WorkerTiming {
            error_backoff,
            idle_timeout,
            cleanup_interval: CLEANUP_INTERVAL,
        }
    }

    fn spawn_worker(
        worker: ScriptedWorker,
        wake: WorkerWake,
        idle_timeout: Duration,
        error_backoff: Duration,
    ) -> (
        ScriptedWorker,
        watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let observe = worker.clone();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(
            worker,
            wake,
            shutdown_rx,
            timing(idle_timeout, error_backoff),
        ));
        (observe, shutdown_tx, task)
    }

    async fn until(mut ready: impl FnMut() -> bool) {
        for _ in 0..200 {
            if ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("worker did not reach the expected state");
    }

    async fn stop(shutdown: watch::Sender<bool>, task: tokio::task::JoinHandle<()>) {
        let _ = shutdown.send(true);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn notify_during_an_empty_claim_claims_again_without_the_fallback() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.notify_during_call(1);
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.finished() >= 2 && worker.resume_calls() >= 1).await;

        assert_eq!(worker.finished(), 2);
        assert_eq!(started.elapsed(), Duration::ZERO);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_wake_interrupts_an_idle_sleep() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) =
            spawn_worker(worker, wake.clone(), idle, Duration::from_secs(30));

        until(|| worker.resume_calls() >= 1).await;
        assert_eq!(worker.finished(), 1);

        wake.wake();
        until(|| worker.finished() >= 2 && worker.resume_calls() >= 2).await;

        assert_eq!(started.elapsed(), Duration::ZERO);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn scheduled_resume_wakes_before_the_idle_fallback() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        let resume_in = Duration::from_secs(2);
        worker.push_resume(Ok(Some(resume_in)));
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.resume_calls() >= 1).await;
        assert_eq!(worker.finished(), 1);

        tokio::time::advance(resume_in - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(worker.finished(), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        until(|| worker.finished() >= 2 && worker.resume_calls() >= 2).await;
        assert!(started.elapsed() < idle);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn later_resume_is_capped_by_the_idle_fallback() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.push_resume(Ok(Some(Duration::from_secs(60 * 60))));
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.resume_calls() >= 1).await;
        tokio::time::advance(idle - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(worker.finished(), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        until(|| worker.finished() >= 2).await;
        assert_eq!(started.elapsed(), idle);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn empty_queue_sleeps_until_the_fallback() {
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.resume_calls() >= 1).await;
        tokio::time::advance(idle).await;
        until(|| worker.resume_calls() >= 2 && worker.finished() >= 2).await;
        assert_eq!(worker.cleanup_calls(), 1);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn due_job_is_retried_without_waiting_for_the_fallback() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.push_resume(Ok(Some(Duration::ZERO)));
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.finished() >= 2 && worker.resume_calls() >= 2).await;
        assert_eq!(worker.finished(), 2);
        assert_eq!(started.elapsed(), Duration::ZERO);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn claimed_job_is_followed_by_another_claim_without_idle_sleep() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.push_process(Ok(true));
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.finished() >= 2 && worker.resume_calls() >= 1).await;
        assert_eq!(worker.finished(), 2);
        assert_eq!(worker.resume_calls(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn iteration_failure_backs_off_and_a_wake_cuts_the_backoff_short() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.push_process(Err("database unavailable"));
        let backoff = Duration::from_secs(30);
        let (worker, shutdown, task) =
            spawn_worker(worker, wake.clone(), Duration::from_secs(900), backoff);

        until(|| worker.finished() >= 1).await;
        assert_eq!(worker.resume_calls(), 0);
        wake.wake();
        until(|| worker.finished() >= 2 && worker.resume_calls() >= 1).await;
        assert!(started.elapsed() < backoff);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn schedule_read_failure_uses_the_error_backoff() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        worker.push_resume(Err("database unavailable"));
        let backoff = Duration::from_secs(5);
        let idle = Duration::from_secs(900);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, backoff);

        until(|| worker.resume_calls() >= 1 && worker.finished() >= 1).await;
        tokio::time::advance(backoff - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(worker.finished(), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        until(|| worker.finished() >= 2).await;
        assert!(started.elapsed() < idle);
        stop(shutdown, task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_idle_sleep_stops_the_worker() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        let (worker, shutdown, task) = spawn_worker(
            worker,
            wake,
            Duration::from_secs(900),
            Duration::from_secs(30),
        );

        until(|| worker.resume_calls() >= 1).await;
        stop(shutdown, task).await;
        assert_eq!(worker.finished(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_rides_along_with_real_wakeups() {
        let started = tokio::time::Instant::now();
        let wake = WorkerWake::new();
        let worker = ScriptedWorker::new(wake.clone());
        let idle = Duration::from_secs(15 * 60);
        let (worker, shutdown, task) = spawn_worker(worker, wake, idle, Duration::from_secs(30));

        until(|| worker.resume_calls() >= 1).await;
        assert_eq!(worker.cleanup_calls(), 1);

        for expected_resume in 2..=4 {
            tokio::time::advance(idle).await;
            until(|| worker.resume_calls() >= expected_resume).await;
            assert_eq!(worker.cleanup_calls(), 1);
        }

        tokio::time::advance(idle).await;
        until(|| worker.resume_calls() >= 5).await;
        assert_eq!(worker.cleanup_calls(), 2);
        assert_eq!(started.elapsed(), Duration::from_secs(60 * 60));
        stop(shutdown, task).await;
    }
}
