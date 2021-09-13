use crossbeam_deque::{Injector, Stealer, Worker};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(crate) fn run_recursive_job<IN, OUT, JOB>(initial: Vec<IN>, job: JOB, num_workers: usize) -> Vec<OUT>
where
    IN: Send,
    OUT: Send,
    JOB: Fn(IN, &Worker<IN>) -> Option<OUT> + Clone + Send,
{
    // Create crossbeam_deque injector/worker/stealers
    let injector = Injector::new();
    let workers: Vec<_> = (0..num_workers).map(|_| Worker::new_lifo()).collect();
    let stealers: Vec<_> = workers.iter().map(|w| w.stealer()).collect();
    let active_counter = ActiveCounter::new();

    // Seed injector with initial data
    for item in initial.into_iter() {
        injector.push(item);
    }

    // Create single scope to contain all workers
    let result: Vec<OUT> = crossbeam_utils::thread::scope(|scope| {
        // Container for all workers
        let mut worker_scopes: Vec<_> = Default::default();

        // Create each worker
        for worker in workers.into_iter() {
            // Make copy of data so we can move clones or references into closure
            let injector_borrow = &injector;
            let stealers_copy = stealers.clone();
            let job_copy = job.clone();
            let mut counter_copy = active_counter.clone();

            // Create scope for single worker
            let s = scope.spawn(move |_| {
                // results of this worker
                let mut worker_results: Vec<_> = Default::default();

                // backoff spinner for sleeping
                let backoff = crossbeam_utils::Backoff::new();

                // Loop until all workers idle
                loop {
                    {
                        // look for work
                        let _ = counter_copy.take_token();
                        while let Some(item) = find_task(&worker, injector_borrow, &stealers_copy) {
                            backoff.reset();

                            // do work
                            if let Some(result) = job_copy(item, &worker) {
                                worker_results.push(result);
                            }
                        }
                    }

                    // no work, check if all workers are idle
                    if counter_copy.is_zero() {
                        break;
                    }

                    // sleep
                    backoff.snooze();
                }

                worker_results
            });

            worker_scopes.push(s);
        }

        // run all workers to completion and combine their results
        worker_scopes
            .into_iter()
            .filter_map(|s| s.join().ok())
            .flatten()
            .collect()
    })
    .unwrap();

    result
}

fn find_task<T>(local: &Worker<T>, global: &Injector<T>, stealers: &[Stealer<T>]) -> Option<T> {
    // Pop a task from the local queue, if not empty.
    local.pop().or_else(|| {
        // Otherwise, we need to look for a task elsewhere.
        std::iter::repeat_with(|| {
            // Try stealing a batch of tasks from the global queue.
            global
                .steal_batch_and_pop(local)
                // Or try stealing a task from one of the other threads.
                .or_else(|| stealers.iter().map(|s| s.steal()).collect())
        })
        // Loop while no task was stolen and any steal operation needs to be retried.
        .find(|s| !s.is_retry())
        // Extract the stolen task, if there is one.
        .and_then(|s| s.success())
    })
}

// Helpers to track when all workers are done
#[derive(Clone)]
struct ActiveCounter {
    active_count: Arc<AtomicUsize>,
}

impl ActiveCounter {
    pub fn take_token(&mut self) -> ActiveToken {
        self.active_count.fetch_add(1, Ordering::SeqCst);
        ActiveToken {
            active_count: self.active_count.clone(),
        }
    }

    pub fn new() -> ActiveCounter {
        ActiveCounter {
            active_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn is_zero(&self) -> bool {
        self.active_count.load(Ordering::SeqCst) == 0
    }
}

struct ActiveToken {
    active_count: Arc<AtomicUsize>,
}

impl Drop for ActiveToken {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(value: i64, worker: &Worker<i64>) -> Option<i64> {
        if value > 0 {
            worker.push(value - 1);
            Some(value)
        } else {
            None
        }
    }

    fn instant_sum(value: i64) -> i64 {
        (value * (value + 1)) / 2
    }

    fn instant_sums(values: &[i64]) -> i64 {
        values.iter().map(|v| instant_sum(*v)).sum()
    }

    fn recursive_sum(value: i64, num_threads: usize) -> i64 {
        let data = vec![value];
        let results = run_recursive_job(data, job, num_threads);
        results.iter().sum()
    }

    fn recursive_sums(values: &[i64], num_threads: usize) -> i64 {
        let data: Vec<_> = values.iter().cloned().collect();
        let results = run_recursive_job(data, job, num_threads);
        results.iter().sum()
    }

    #[test]
    fn single_threaded_sum() {
        assert_eq!(instant_sum(10), recursive_sum(10, 1));
        assert_eq!(instant_sum(100), recursive_sum(100, 1));
        assert_eq!(instant_sum(1000), recursive_sum(1000, 1));
        assert_eq!(instant_sum(10000), recursive_sum(10000, 1));
    }

    #[test]
    fn single_threaded_sums() {
        let data = vec![10, 100, 1000, 10000];
        assert_eq!(instant_sums(&data), recursive_sums(&data, 1));
    }

    #[test]
    fn multi_threaded_sum() {
        assert_eq!(instant_sum(10), recursive_sum(10, 6));
        assert_eq!(instant_sum(100), recursive_sum(100, 6));
        assert_eq!(instant_sum(1000), recursive_sum(1000, 6));
        assert_eq!(instant_sum(10000), recursive_sum(10000, 6));
    }

    #[test]
    fn multi_threaded_sums() {
        let data = vec![10, 100, 1000, 10000];
        assert_eq!(instant_sums(&data), recursive_sums(&data, 6));
    }

    #[test]
    fn single_threaded_stress() {
        let data: Vec<_> = (0..6000).collect();
        assert_eq!(instant_sums(&data), recursive_sums(&data, 1));
    }

    #[test]
    fn multi_threaded_stress() {
        let data: Vec<_> = (0..10000).collect();
        assert_eq!(instant_sums(&data), recursive_sums(&data, 6));
    }
}
