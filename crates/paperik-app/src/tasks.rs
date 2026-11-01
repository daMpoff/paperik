//! One owned background task at a time. No HWND, State borrow or D2D resource crosses threads.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};

type Work<T> = Box<dyn FnOnce(&AtomicBool) -> Result<T, String> + Send>;
pub struct Completed<T> {
    pub result: Result<T, String>,
    pub cancelled: bool,
}
pub struct Background<T> {
    sender: Option<SyncSender<(Arc<AtomicBool>, Work<T>)>>,
    results: Receiver<Result<T, String>>,
    active: Option<Arc<AtomicBool>>,
}
impl<T: Send + 'static> Background<T> {
    pub fn new() -> Result<Self, String> {
        let (sender, jobs) = mpsc::sync_channel::<(Arc<AtomicBool>, Work<T>)>(1);
        let (report, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("paperik-document".into())
            .spawn(move || {
                while let Ok((cancel, work)) = jobs.recv() {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&cancel)))
                            .unwrap_or_else(|_| {
                                Err("Фоновая операция завершилась с ошибкой".into())
                            });
                    if report.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender: Some(sender),
            results,
            active: None,
        })
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn start(
        &mut self,
        work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("Предыдущая операция ещё выполняется".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.sender
            .as_ref()
            .ok_or("Фоновый поток остановлен")?
            .try_send((cancel.clone(), Box::new(work)))
            .map_err(|_| "Фоновый поток недоступен")?;
        self.active = Some(cancel);
        Ok(())
    }
    pub fn cancel(&self) {
        if let Some(cancel) = &self.active {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn poll(&mut self) -> Option<Completed<T>> {
        let result = match self.results.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) if self.active.is_some() => {
                Err("Фоновый поток остановился".into())
            }
            Err(_) => return None,
        };
        let cancelled = self
            .active
            .take()
            .is_some_and(|v| v.load(Ordering::Relaxed));
        Some(Completed { result, cancelled })
    }
}
impl<T> Drop for Background<T> {
    fn drop(&mut self) {
        if let Some(cancel) = &self.active {
            cancel.store(true, Ordering::Relaxed);
        }
        self.sender.take();
        // The thread owns all work data. Dropping the window never joins blocked I/O.
    }
}
pub fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Операция отменена".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn finish<T: Send + 'static>(work: &mut Background<T>) -> Completed<T> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(done) = work.poll() {
                return done;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn queue_is_bounded_and_cancellation_marks_even_a_finished_result() {
        let mut work = Background::new().unwrap();
        let (release, gate) = mpsc::channel();
        let (ready, arrived) = mpsc::channel();
        work.start(move |_| {
            ready.send(()).unwrap();
            gate.recv().unwrap();
            Ok(42)
        })
        .unwrap();
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(work.start(|_| Ok(7)).is_err());
        assert!(work.poll().is_none());
        work.cancel();
        release.send(()).unwrap();
        let done = finish(&mut work);
        assert!(done.cancelled);
        assert_eq!(done.result.unwrap(), 42);
        work.start(|_| Ok(7)).unwrap();
        assert_eq!(finish(&mut work).result.unwrap(), 7);
    }
    #[test]
    fn dropping_the_window_executor_does_not_join_blocked_io() {
        let mut work: Background<()> = Background::new().unwrap();
        let (release, gate) = mpsc::channel();
        let (ready, arrived) = mpsc::channel();
        work.start(move |_| {
            ready.send(()).unwrap();
            gate.recv().unwrap();
            Ok(())
        })
        .unwrap();
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        let (dropped, observed) = mpsc::channel();
        std::thread::spawn(move || {
            drop(work);
            dropped.send(()).unwrap();
        });
        let result = observed.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        result.unwrap();
    }
}
