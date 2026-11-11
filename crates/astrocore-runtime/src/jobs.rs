use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// Токен отмены: задание проверяет его в безопасных точках и завершается раньше.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Исход задания, который видит хозяин при `poll`.
#[derive(Debug, PartialEq)]
pub enum JobOutcome<T> {
    /// Результат актуален: ревизия мира с запуска не изменилась — можно публиковать.
    Ready { key: String, value: T },
    /// Мир изменился за время работы (или задание отменено): результат отброшен.
    Stale { key: String },
    /// Задание завершилось паникой; сервер продолжает работу.
    Failed { key: String },
}

struct Finished<T> {
    id: u64,
    key: String,
    base_revision: u64,
    cancelled: bool,
    result: Option<T>,
}

struct Running {
    id: u64,
    key: String,
    token: CancelToken,
    thread: Option<JoinHandle<()>>,
}

/// Фоновые вычисления (пересборка мешей и навигации, импорт) с отменой и проверкой ревизии
/// перед публикацией. Одно задание на ключ: новое с тем же ключом отменяет предыдущее.
pub struct JobSet<T: Send + 'static> {
    tx: Sender<Finished<T>>,
    rx: Receiver<Finished<T>>,
    running: Vec<Running>,
    next_id: u64,
}

impl<T: Send + 'static> Default for JobSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + 'static> JobSet<T> {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            running: Vec::new(),
            next_id: 0,
        }
    }

    /// Запустить задание над данными ревизии `base_revision`.
    pub fn spawn(
        &mut self,
        key: impl Into<String>,
        base_revision: u64,
        work: impl FnOnce(&CancelToken) -> T + Send + 'static,
    ) {
        let key = key.into();
        // Предыдущее задание того же ключа больше не нужно.
        for r in self.running.iter().filter(|r| r.key == key) {
            r.token.cancel();
        }
        let id = self.next_id;
        self.next_id += 1;
        let token = CancelToken::default();
        let tx = self.tx.clone();
        let (job_key, job_token) = (key.clone(), token.clone());
        let thread = std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&job_token)));
            let _ = tx.send(Finished {
                id,
                key: job_key,
                base_revision,
                cancelled: job_token.is_cancelled(),
                result: result.ok(),
            });
        });
        self.running.push(Running {
            id,
            key,
            token,
            thread: Some(thread),
        });
    }

    pub fn cancel(&self, key: &str) {
        for r in self.running.iter().filter(|r| r.key == key) {
            r.token.cancel();
        }
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Забрать завершённые задания. Результат публикуется, только если `current_revision` равна
    /// ревизии, на которой задание начиналось, и оно не отменено.
    pub fn poll(&mut self, current_revision: u64) -> Vec<JobOutcome<T>> {
        let mut out = Vec::new();
        while let Ok(done) = self.rx.try_recv() {
            if let Some(i) = self.running.iter().position(|r| r.id == done.id) {
                let mut r = self.running.remove(i);
                if let Some(t) = r.thread.take() {
                    let _ = t.join();
                }
            }
            out.push(match done.result {
                None => JobOutcome::Failed { key: done.key },
                Some(_) if done.cancelled || done.base_revision != current_revision => {
                    JobOutcome::Stale { key: done.key }
                }
                Some(value) => JobOutcome::Ready {
                    key: done.key,
                    value,
                },
            });
        }
        out
    }

    /// Дождаться всех заданий (остановка сервера, тесты).
    pub fn join_all(&mut self, current_revision: u64) -> Vec<JobOutcome<T>> {
        for r in &mut self.running {
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
        let out = self.poll(current_revision);
        self.running.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn fresh_results_are_published_and_stale_ones_dropped() {
        let mut jobs: JobSet<u32> = JobSet::new();
        jobs.spawn("nav:0", 7, |_| 42);
        jobs.spawn("mesh:3", 7, |_| 5);
        let mut out = jobs.join_all(7);
        out.sort_by_key(|o| match o {
            JobOutcome::Ready { key, .. }
            | JobOutcome::Stale { key }
            | JobOutcome::Failed { key } => key.clone(),
        });
        assert_eq!(
            out,
            vec![
                JobOutcome::Ready {
                    key: "mesh:3".into(),
                    value: 5
                },
                JobOutcome::Ready {
                    key: "nav:0".into(),
                    value: 42
                },
            ]
        );
        // Мир изменился, пока задание считалось: публиковать нельзя.
        jobs.spawn("nav:0", 7, |_| 1);
        let out = jobs.join_all(8);
        assert_eq!(
            out,
            vec![JobOutcome::Stale {
                key: "nav:0".into()
            }]
        );
    }

    #[test]
    fn a_new_job_with_the_same_key_cancels_the_old_one() {
        let mut jobs: JobSet<&'static str> = JobSet::new();
        jobs.spawn("k", 1, |t| {
            for _ in 0..500 {
                if t.is_cancelled() {
                    return "cancelled";
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            "finished"
        });
        std::thread::sleep(Duration::from_millis(20));
        jobs.spawn("k", 1, |_| "second");
        let out = jobs.join_all(1);
        assert!(
            out.contains(&JobOutcome::Stale { key: "k".into() }),
            "{out:?}"
        );
        assert!(out.contains(&JobOutcome::Ready {
            key: "k".into(),
            value: "second"
        }));
    }

    #[test]
    fn a_panicking_job_is_reported_without_taking_the_host_down() {
        let mut jobs: JobSet<u8> = JobSet::new();
        jobs.spawn("bad", 1, |_| panic!("ошибка в задании"));
        jobs.spawn("good", 1, |_| 1);
        let out = jobs.join_all(1);
        assert!(out.contains(&JobOutcome::Failed { key: "bad".into() }));
        assert!(out.contains(&JobOutcome::Ready {
            key: "good".into(),
            value: 1
        }));
        assert_eq!(jobs.running(), 0);
    }
}
