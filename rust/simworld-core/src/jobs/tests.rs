//! Job pool tests.

use super::*;
use crossbeam_channel::{unbounded, Receiver};

fn pool(n: usize) -> JobPool {
    JobPool::new(PoolConfig::new(n, "test-jobs")).unwrap()
}

/// A job that blocks its worker until `gate` is dropped / sent to.
fn blocker(p: &JobPool, id: JobId) -> crossbeam_channel::Sender<()> {
    let (tx, rx) = unbounded::<()>();
    let (started_tx, started_rx) = unbounded::<()>();
    p.submit(
        JobSpec::new(id, Priority::on_demand(0.0)),
        move |_| {
            let _ = started_tx.send(());
            let _ = rx.recv();
            JobEnd::Done(None)
        },
        || {},
    );
    started_rx.recv_timeout(Duration::from_secs(5)).expect("blocker started");
    tx
}

fn wait_for(rx: &Receiver<(JobId, &'static str)>, n: usize) -> Vec<(JobId, &'static str)> {
    (0..n).map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("job end")).collect()
}

fn record(p: &JobPool, spec: JobSpec, tx: &crossbeam_channel::Sender<(JobId, &'static str)>) {
    let id = spec.id;
    let (a, b) = (tx.clone(), tx.clone());
    p.submit(
        spec,
        move |_| {
            let _ = a.send((id, "run"));
            JobEnd::Done(None)
        },
        move || {
            let _ = b.send((id, "cancel"));
        },
    );
}

#[test]
fn pool_size_is_the_configured_thread_count() {
    assert_eq!(pool(3).threads(), 3);
    assert_eq!(JobPool::new(PoolConfig::new(0, "z")).unwrap().threads(), 1);
    let d = worker_threads(0);
    assert!((2..=AUTO_THREADS_MAX).contains(&d), "cores - 2 clamped to 2..=8: {d}");
    assert_eq!([1, 4, 6, 10, 12, 64].map(auto_threads), [2, 2, 4, 8, 8, 8]);
    assert_eq!(worker_threads(3), 3);
    // The thread start hook runs once per thread.
    let n = Arc::new(AtomicU64::new(0));
    let n2 = n.clone();
    let mut cfg = PoolConfig::new(4, "hook");
    cfg.on_thread_start = Some(Arc::new(move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
    }));
    let p = JobPool::new(cfg).unwrap();
    assert!(p.shutdown(Duration::from_secs(2)));
    assert_eq!(n.load(Ordering::SeqCst), 4);
}

#[test]
fn on_demand_overtakes_prefetch_then_order_then_fifo() {
    let p = pool(1);
    let gate = blocker(&p, 1000);
    let (tx, rx) = unbounded();
    record(&p, JobSpec::new(1, Priority::prefetch(0.0)), &tx);
    record(&p, JobSpec::new(2, Priority::prefetch(-5.0)), &tx);
    record(&p, JobSpec::new(3, Priority::on_demand(900.0)), &tx);
    record(&p, JobSpec::new(4, Priority::on_demand(10.0)), &tx);
    record(&p, JobSpec::new(5, Priority::on_demand(10.0)), &tx);
    record(&p, JobSpec::new(6, Priority::on_demand(f64::NAN)), &tx);
    assert_eq!(p.stats().queued, 6);
    assert_eq!(p.stats().running, 1);
    drop(gate);
    let order: Vec<JobId> = wait_for(&rx, 6).into_iter().map(|(id, _)| id).collect();
    assert_eq!(order, vec![4, 5, 3, 6, 2, 1]);
}

#[test]
fn cancel_removes_queued_jobs_and_stops_running_ones() {
    let p = pool(1);
    let (tx, rx) = unbounded();
    // A running job that loops until cancelled (a safe point per iteration).
    let (started_tx, started_rx) = unbounded::<()>();
    let (a, b) = (tx.clone(), tx.clone());
    p.submit(
        JobSpec::new(1, Priority::on_demand(0.0)),
        move |ctx| {
            let _ = started_tx.send(());
            let t0 = Instant::now();
            while !ctx.is_cancelled() {
                assert!(t0.elapsed() < Duration::from_secs(5));
                std::thread::yield_now();
            }
            let _ = a.send((1, "run-stopped"));
            JobEnd::Cancelled
        },
        move || {
            let _ = b.send((1, "cancel"));
        },
    );
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    record(&p, JobSpec::new(2, Priority::on_demand(0.0)), &tx);
    record(&p, JobSpec::new(3, Priority::on_demand(0.0)), &tx);
    p.cancel(&[2, 99]);
    assert_eq!(wait_for(&rx, 1), vec![(2, "cancel")], "queued job ends at once");
    p.cancel(&[1]);
    let ends = wait_for(&rx, 3);
    assert_eq!(ends, vec![(1, "run-stopped"), (1, "cancel"), (3, "run")]);
    assert_eq!(p.stats(), JobStats { queued: 0, running: 0, cancelled: 2 });
    // The same id can be submitted again after its end (a new request of the tile).
    record(&p, JobSpec::new(2, Priority::on_demand(0.0)), &tx);
    assert_eq!(wait_for(&rx, 1), vec![(2, "run")]);
}

/// Review L1: a dependant submitted after its dependency ended `Done` (the decode finished
/// between the scheduler's dispatch and the submit) runs with the output while someone still
/// holds it (the store / the reply message), instead of being cancelled and redone; once the
/// output is gone it is cancelled as before.
#[test]
fn dependant_submitted_after_its_dependency_finished_takes_the_live_output() {
    let p = pool(1);
    let (out_tx, out_rx) = unbounded::<Output>();
    p.submit(
        JobSpec::new(1, Priority::on_demand(0.0)),
        move |_| {
            let o: Output = Arc::new(41u32);
            let _ = out_tx.send(o.clone()); // the "reply message" keeps it alive
            JobEnd::Done(Some(o))
        },
        || {},
    );
    let held = out_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let t0 = Instant::now();
    while p.stats().running > 0 || p.stats().queued > 0 {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::yield_now();
    }
    let (tx, rx) = unbounded();
    let (a, b) = (tx.clone(), tx.clone());
    assert!(p.submit(
        JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]),
        move |ctx| {
            let v = ctx.deps().first().and_then(|o| o.clone().downcast::<u32>().ok()).map(|v| *v);
            let _ = a.send((2, if v == Some(41) { "run-41" } else { "run-bad" }));
            JobEnd::Done(None)
        },
        move || {
            let _ = b.send((2, "cancel"));
        },
    ));
    assert_eq!(wait_for(&rx, 1), vec![(2, "run-41")]);
    assert_eq!(p.stats().cancelled, 0);
    // Output dropped: the dependency is unknown again.
    drop(held);
    record(&p, JobSpec::new(3, Priority::on_demand(0.0)).with_deps(vec![1]), &tx);
    assert_eq!(wait_for(&rx, 1), vec![(3, "cancel")]);
}

#[test]
fn dependent_runs_after_its_dependency_with_its_output() {
    let p = pool(3);
    let (tx, rx) = unbounded::<(JobId, &'static str)>();
    let gate = blocker(&p, 500);
    let (dep_go_tx, dep_go_rx) = unbounded::<()>();
    let (started_tx, started_rx) = unbounded::<()>();
    let a = tx.clone();
    p.submit(
        JobSpec::new(1, Priority::prefetch(0.0)),
        move |_| {
            let _ = started_tx.send(());
            let _ = dep_go_rx.recv();
            let _ = a.send((1, "run"));
            JobEnd::Done(Some(Arc::new(41u32) as Output))
        },
        || {},
    );
    let (b, c) = (tx.clone(), tx.clone());
    p.submit(
        JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]).with_version(7),
        move |ctx| {
            assert_eq!(ctx.version(), 7);
            assert_eq!(ctx.dep::<u32>(0), Some(&41));
            let _ = b.send((2, "run"));
            JobEnd::Done(None)
        },
        move || {
            let _ = c.send((2, "cancel"));
        },
    );
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(p.stats(), JobStats { queued: 1, running: 2, cancelled: 0 }, "the dependant waits");
    std::thread::sleep(Duration::from_millis(50));
    assert!(rx.try_recv().is_err(), "nothing ran before the dependency");
    dep_go_tx.send(()).unwrap();
    assert_eq!(wait_for(&rx, 2), vec![(1, "run"), (2, "run")]);
    drop(gate);
    // Unknown dependency: cancelled at once.
    record(&p, JobSpec::new(3, Priority::on_demand(0.0)).with_deps(vec![12345]), &tx);
    assert_eq!(wait_for(&rx, 1), vec![(3, "cancel")]);
}

#[test]
fn cancelling_a_dependency_cancels_its_dependants() {
    let p = pool(1);
    let (tx, rx) = unbounded();
    let gate = blocker(&p, 500);
    record(&p, JobSpec::new(1, Priority::on_demand(0.0)), &tx);
    record(&p, JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]), &tx);
    record(&p, JobSpec::new(3, Priority::on_demand(0.0)).with_deps(vec![2]), &tx);
    record(&p, JobSpec::new(4, Priority::on_demand(0.0)), &tx);
    p.cancel(&[1]);
    let mut ends = wait_for(&rx, 3);
    ends.sort();
    assert_eq!(ends, vec![(1, "cancel"), (2, "cancel"), (3, "cancel")]);
    drop(gate);
    assert_eq!(wait_for(&rx, 1), vec![(4, "run")]);
    assert_eq!(p.stats().cancelled, 3);
    // A dependency that is running when cancelled: the dependant ends at once, the
    // dependency at its next safe point.
    let (go_tx, go_rx) = unbounded::<()>();
    let (started_tx, started_rx) = unbounded::<()>();
    let a = tx.clone();
    p.submit(
        JobSpec::new(10, Priority::on_demand(0.0)),
        move |ctx| {
            let _ = started_tx.send(());
            let _ = go_rx.recv();
            if ctx.is_cancelled() {
                return JobEnd::Cancelled;
            }
            let _ = a.send((10, "run"));
            JobEnd::Done(None)
        },
        {
            let b = tx.clone();
            move || {
                let _ = b.send((10, "cancel"));
            }
        },
    );
    record(&p, JobSpec::new(11, Priority::on_demand(0.0)).with_deps(vec![10]), &tx);
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    p.cancel(&[10]);
    assert_eq!(wait_for(&rx, 1), vec![(11, "cancel")]);
    go_tx.send(()).unwrap();
    assert_eq!(wait_for(&rx, 1), vec![(10, "cancel")]);
}

#[test]
fn a_panicking_job_still_ends_once_and_fails_its_dependants() {
    let p = pool(1);
    let (tx, rx) = unbounded();
    let b = tx.clone();
    p.submit(
        JobSpec::new(1, Priority::on_demand(0.0)),
        |_| panic!("boom"),
        move || {
            let _ = b.send((1, "cancel"));
        },
    );
    record(&p, JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]), &tx);
    let mut ends = wait_for(&rx, 2);
    ends.sort();
    assert_eq!(ends, vec![(1, "cancel"), (2, "cancel")]);
    record(&p, JobSpec::new(3, Priority::on_demand(0.0)), &tx);
    assert_eq!(wait_for(&rx, 1), vec![(3, "run")], "the thread survived");
}

#[test]
fn shutdown_cancels_everything_and_never_deadlocks() {
    let p = pool(2);
    let (tx, rx) = unbounded();
    let g1 = blocker(&p, 100);
    let g2 = blocker(&p, 101);
    for i in 0..50 {
        record(&p, JobSpec::new(i, Priority::on_demand(i as f64)), &tx);
    }
    // Running jobs are stuck: the bounded wait gives up and reports it.
    assert!(!p.shutdown(Duration::from_millis(50)));
    let ends = wait_for(&rx, 50);
    assert!(ends.iter().all(|e| e.1 == "cancel"));
    // Submitting after shutdown cancels at once.
    record(&p, JobSpec::new(77, Priority::on_demand(0.0)), &tx);
    assert_eq!(wait_for(&rx, 1), vec![(77, "cancel")]);
    drop((g1, g2));
    assert!(p.shutdown(Duration::from_secs(2)), "threads exit once their jobs end");
    assert!(p.shutdown(Duration::from_millis(1)), "twice is fine");
    // Dropping a busy pool does not block.
    let q = pool(1);
    let _g = blocker(&q, 1);
    let t0 = Instant::now();
    drop(q);
    assert!(t0.elapsed() < Duration::from_millis(500));
}
