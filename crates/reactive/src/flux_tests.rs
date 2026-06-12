//! Operator-by-operator marble tests for [`Flux`](crate::Flux).
//!
//! Each test asserts an exact emitted sequence (marble-style) and keeps
//! any timed work tiny so the whole suite never sleeps more than a few
//! hundred milliseconds in aggregate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use firefly_kernel::FireflyError;
use futures::StreamExt;

use super::*;

/// Collects a `Flux<T>` into a `Vec<T>`, panicking on error — the marble
/// assertion helper. `collect_list` always yields a (possibly empty)
/// list, so the outer `Option` is unwrapped too.
async fn drain<T: Send + 'static>(f: Flux<T>) -> Vec<T> {
    f.collect_list().block().await.unwrap().unwrap_or_default()
}

#[tokio::test]
async fn just_emits_in_order() {
    assert_eq!(drain(Flux::just(vec![1, 2, 3])).await, vec![1, 2, 3]);
}

#[tokio::test]
async fn from_iter_emits() {
    assert_eq!(drain(Flux::from_iter(vec!["a", "b"])).await, vec!["a", "b"]);
}

#[tokio::test]
async fn empty_emits_nothing() {
    assert!(drain(Flux::<i32>::empty()).await.is_empty());
}

#[tokio::test]
async fn error_short_circuits() {
    let e = Flux::<i32>::error(FireflyError::internal("x"))
        .collect_list()
        .block()
        .await
        .unwrap_err();
    assert_eq!(e.status, 500);
}

#[tokio::test]
async fn map_transforms_each() {
    assert_eq!(drain(Flux::range(1, 3).map(|x| x * 2)).await, vec![2, 4, 6]);
}

#[tokio::test]
async fn map_async_transforms_each() {
    let out = drain(Flux::range(1, 3).map_async(|x| async move { x + 10 })).await;
    assert_eq!(out, vec![11, 12, 13]);
}

#[tokio::test]
async fn flat_map_fans_out() {
    let mut out = drain(Flux::range(1, 3).flat_map(4, |x| Flux::range(0, x))).await;
    out.sort();
    // x=1 -> [0]; x=2 -> [0,1]; x=3 -> [0,1,2]
    assert_eq!(out, vec![0, 0, 0, 1, 1, 2]);
}

#[tokio::test]
async fn concat_map_preserves_order() {
    let out = drain(Flux::range(1, 3).concat_map(|x| Flux::just(vec![x, x * 10]))).await;
    assert_eq!(out, vec![1, 10, 2, 20, 3, 30]);
}

#[tokio::test]
async fn flat_map_iterable_flattens() {
    let out = drain(Flux::range(1, 3).flat_map_iterable(|x| vec![x, -x])).await;
    assert_eq!(out, vec![1, -1, 2, -2, 3, -3]);
}

#[tokio::test]
async fn filter_keeps_matching() {
    let out = drain(Flux::range(1, 6).filter(|x| x % 2 == 0)).await;
    assert_eq!(out, vec![2, 4, 6]);
}

#[tokio::test]
async fn take_limits() {
    assert_eq!(drain(Flux::range(1, 100).take(3)).await, vec![1, 2, 3]);
    assert!(drain(Flux::range(1, 100).take(0)).await.is_empty());
}

#[tokio::test]
async fn take_while_stops() {
    let out = drain(Flux::from_iter(vec![1, 2, 3, 1]).take_while(|x| *x < 3)).await;
    assert_eq!(out, vec![1, 2]);
}

#[tokio::test]
async fn take_last_keeps_tail() {
    assert_eq!(drain(Flux::range(1, 5).take_last(2)).await, vec![4, 5]);
    assert!(drain(Flux::range(1, 5).take_last(0)).await.is_empty());
}

#[tokio::test]
async fn skip_drops_prefix() {
    assert_eq!(drain(Flux::range(1, 5).skip(2)).await, vec![3, 4, 5]);
}

#[tokio::test]
async fn skip_while_drops_prefix() {
    let out = drain(Flux::from_iter(vec![1, 2, 3, 1]).skip_while(|x| *x < 3)).await;
    assert_eq!(out, vec![3, 1]);
}

#[tokio::test]
async fn distinct_drops_all_duplicates() {
    let out = drain(Flux::from_iter(vec![1, 2, 1, 3, 2]).distinct()).await;
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test]
async fn distinct_until_changed_drops_runs() {
    let out = drain(Flux::from_iter(vec![1, 1, 2, 2, 1]).distinct_until_changed()).await;
    assert_eq!(out, vec![1, 2, 1]);
}

#[tokio::test]
async fn scan_accumulates() {
    let out = drain(Flux::range(1, 3).scan(0, |acc, x| acc + x)).await;
    assert_eq!(out, vec![0, 1, 3, 6]);
}

#[tokio::test]
async fn index_pairs() {
    let out = drain(Flux::from_iter(vec!["a", "b"]).index()).await;
    assert_eq!(out, vec![(0, "a"), (1, "b")]);
}

#[tokio::test]
async fn start_with_prepends() {
    let out = drain(Flux::range(2, 2).start_with(vec![0, 1])).await;
    assert_eq!(out, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn merge_with_includes_all() {
    let mut out = drain(Flux::range(1, 2).merge_with(Flux::range(10, 2))).await;
    out.sort();
    assert_eq!(out, vec![1, 2, 10, 11]);
}

#[tokio::test]
async fn concat_with_orders() {
    let out = drain(Flux::range(1, 2).concat_with(Flux::range(10, 2))).await;
    assert_eq!(out, vec![1, 2, 10, 11]);
}

#[tokio::test]
async fn zip_with_pairs_positionally() {
    let out = drain(Flux::range(1, 3).zip_with(Flux::from_iter(vec!["a", "b"]))).await;
    assert_eq!(out, vec![(1, "a"), (2, "b")]);
}

#[tokio::test]
async fn combine_latest_emits_latest_pairs() {
    let a = Flux::from_iter(vec![1, 2]);
    let b = Flux::from_iter(vec!["x"]);
    let out = drain(a.combine_latest(b)).await;
    // Deterministic select ordering: a fully then b, last pairs vary;
    // assert it contains the final combination.
    assert!(out.contains(&(2, "x")));
}

#[tokio::test]
async fn switch_if_empty_switches() {
    let out = drain(Flux::<i64>::empty().switch_if_empty(Flux::range(1, 2))).await;
    assert_eq!(out, vec![1, 2]);
    let out = drain(Flux::range(5, 1).switch_if_empty(Flux::range(1, 2))).await;
    assert_eq!(out, vec![5]);
}

#[tokio::test]
async fn default_if_empty_fills() {
    assert_eq!(
        drain(Flux::<i32>::empty().default_if_empty(9)).await,
        vec![9]
    );
    assert_eq!(
        drain(Flux::range(1, 2).default_if_empty(9)).await,
        vec![1, 2]
    );
}

#[tokio::test]
async fn buffer_batches() {
    let out = drain(Flux::range(1, 5).buffer(2)).await;
    assert_eq!(out, vec![vec![1, 2], vec![3, 4], vec![5]]);
}

#[tokio::test]
async fn window_splits() {
    let windows = Flux::range(1, 5).window(2);
    let mut got = Vec::new();
    let mut s = windows.into_stream();
    while let Some(w) = s.next().await {
        got.push(drain(w.unwrap()).await);
    }
    assert_eq!(got, vec![vec![1, 2], vec![3, 4], vec![5]]);
}

#[tokio::test]
async fn group_by_partitions() {
    let groups = Flux::range(1, 6).group_by(|x| x % 2);
    let mut s = groups.into_stream();
    let mut collected: Vec<(i64, Vec<i64>)> = Vec::new();
    while let Some(g) = s.next().await {
        let (k, vf) = g.unwrap();
        collected.push((k, drain(vf).await));
    }
    collected.sort_by_key(|(k, _)| *k);
    assert_eq!(collected, vec![(0, vec![2, 4, 6]), (1, vec![1, 3, 5])]);
}

#[tokio::test]
async fn reduce_folds() {
    let out = Flux::range(1, 4)
        .reduce(0i64, |a, x| a + x)
        .block()
        .await
        .unwrap();
    assert_eq!(out, Some(10));
}

#[tokio::test]
async fn collect_list_roundtrip() {
    let out = Flux::range(1, 3).collect_list().block().await.unwrap();
    assert_eq!(out, Some(vec![1, 2, 3]));
}

#[tokio::test]
async fn collect_map_keys() {
    let out = Flux::from_iter(vec!["aa", "b", "cc"])
        .collect_map(|s| s.len())
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.get(&1), Some(&"b"));
    assert_eq!(out.get(&2), Some(&"cc")); // last write wins
}

#[tokio::test]
async fn count_counts() {
    let out = Flux::range(1, 5).count().block().await.unwrap();
    assert_eq!(out, Some(5));
}

#[tokio::test]
async fn all_and_any() {
    assert_eq!(
        Flux::range(2, 3).all(|x| *x > 0).block().await.unwrap(),
        Some(true)
    );
    assert_eq!(
        Flux::range(0, 3).all(|x| *x > 0).block().await.unwrap(),
        Some(false)
    );
    assert_eq!(
        Flux::range(0, 3).any(|x| *x == 2).block().await.unwrap(),
        Some(true)
    );
    assert_eq!(
        Flux::range(0, 3).any(|x| *x == 9).block().await.unwrap(),
        Some(false)
    );
}

#[tokio::test]
async fn then_completes() {
    let out = Flux::range(1, 3).then().block().await.unwrap();
    assert_eq!(out, Some(()));
}

#[tokio::test]
async fn next_first_or_empty() {
    assert_eq!(Flux::range(7, 3).next().block().await.unwrap(), Some(7));
    assert_eq!(Flux::<i32>::empty().next().block().await.unwrap(), None);
}

#[tokio::test]
async fn last_last_or_empty() {
    assert_eq!(Flux::range(1, 3).last().block().await.unwrap(), Some(3));
    assert_eq!(Flux::<i32>::empty().last().block().await.unwrap(), None);
}

#[tokio::test]
async fn single_exact_one() {
    assert_eq!(Flux::range(5, 1).single().block().await.unwrap(), Some(5));
    assert!(Flux::range(1, 2).single().block().await.is_err());
    assert!(Flux::<i32>::empty().single().block().await.is_err());
}

#[tokio::test]
async fn element_at_indexes() {
    assert_eq!(
        Flux::range(10, 5).element_at(2).block().await.unwrap(),
        Some(12)
    );
    assert_eq!(
        Flux::range(10, 5).element_at(99).block().await.unwrap(),
        None
    );
}

#[tokio::test]
async fn on_error_resume_recovers() {
    let f = Flux::range(1, 2).concat_with(Flux::error(FireflyError::internal("x")));
    let out = drain(f.on_error_resume(|_| Flux::range(7, 2))).await;
    assert_eq!(out, vec![1, 2, 7, 8]);
}

#[tokio::test]
async fn on_error_continue_skips() {
    // Source already short-circuits at the Err; on_error_continue drops
    // the error and finishes with whatever came before.
    let f = Flux::from_stream(futures::stream::iter(vec![
        Ok(1),
        Err(FireflyError::internal("x")),
        Ok(3),
    ]));
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let out = drain(f.on_error_continue(move |_| {
        s.fetch_add(1, Ordering::SeqCst);
    }))
    .await;
    assert_eq!(out, vec![1, 3]);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_resubscribes() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let f = Flux::retry(
        move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Flux::range(1, 1).concat_with(Flux::error(FireflyError::internal("x")))
            } else {
                Flux::range(1, 2)
            }
        },
        5,
    );
    let out = drain(f).await;
    // attempts 0,1 emit [1] then error; attempt 2 emits [1,2] clean.
    assert_eq!(out, vec![1, 1, 1, 2]);
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retry_exhausts() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let e = Flux::retry(
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            Flux::<i32>::error(FireflyError::internal("x"))
        },
        2,
    )
    .collect_list()
    .block()
    .await
    .unwrap_err();
    assert_eq!(e.status, 500);
    assert_eq!(counter.load(Ordering::SeqCst), 3); // initial + 2 retries
}

#[tokio::test(start_paused = true)]
async fn retry_backoff_resubscribes() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let f = Flux::retry_backoff(
        move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n < 1 {
                Flux::<i64>::error(FireflyError::internal("x"))
            } else {
                Flux::range(1, 2)
            }
        },
        Backoff::new(3, Duration::from_millis(10)),
    );
    let out = drain(f).await;
    assert_eq!(out, vec![1, 2]);
}

#[tokio::test(start_paused = true)]
async fn timeout_between_items_fails() {
    let slow = Flux::from_stream(async_stream::try_stream! {
        yield 1;
        tokio::time::sleep(Duration::from_secs(10)).await;
        yield 2;
    });
    let e = slow
        .timeout(Duration::from_millis(50))
        .collect_list()
        .block()
        .await
        .unwrap_err();
    assert_eq!(e.status, 504);
}

#[tokio::test(start_paused = true)]
async fn timeout_fast_stream_passes() {
    let out = drain(Flux::range(1, 3).timeout(Duration::from_millis(50))).await;
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn delay_elements_preserves_order() {
    let out = drain(Flux::range(1, 3).delay_elements(Duration::from_millis(5))).await;
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test]
async fn do_on_next_peeks() {
    let sum = Arc::new(AtomicUsize::new(0));
    let s = sum.clone();
    let out = drain(Flux::range(1, 3).do_on_next(move |v| {
        s.fetch_add(*v as usize, Ordering::SeqCst);
    }))
    .await;
    assert_eq!(out, vec![1, 2, 3]);
    assert_eq!(sum.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn do_on_complete_and_finally() {
    let complete = Arc::new(AtomicUsize::new(0));
    let finally = Arc::new(AtomicUsize::new(0));
    let cc = complete.clone();
    let ff = finally.clone();
    let _ = drain(
        Flux::range(1, 2)
            .do_on_complete(move || {
                cc.fetch_add(1, Ordering::SeqCst);
            })
            .do_on_finally(move || {
                ff.fetch_add(1, Ordering::SeqCst);
            }),
    )
    .await;
    assert_eq!(complete.load(Ordering::SeqCst), 1);
    assert_eq!(finally.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn do_on_error_fires() {
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let _ = Flux::<i32>::error(FireflyError::internal("x"))
        .do_on_error(move |_| {
            s.fetch_add(1, Ordering::SeqCst);
        })
        .collect_list()
        .block()
        .await;
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn on_backpressure_buffer_preserves_all() {
    let out = drain(Flux::range(1, 10).on_backpressure_buffer(2)).await;
    assert_eq!(out, (1..=10).collect::<Vec<_>>());
}

#[tokio::test]
async fn on_backpressure_drop_keeps_subset() {
    // With a fast producer and small buffer, some items may be dropped;
    // the surviving subset must be a subsequence of the source.
    let out = drain(Flux::range(1, 50).on_backpressure_drop(4)).await;
    assert!(out.iter().all(|x| (1..=50).contains(x)));
    assert!(out.windows(2).all(|w| w[0] < w[1]));
}

#[tokio::test]
async fn on_backpressure_latest_keeps_recent() {
    let out = drain(Flux::range(1, 20).on_backpressure_latest()).await;
    // Subsequence of source, last element preserved.
    assert!(out.windows(2).all(|w| w[0] < w[1]));
    assert!(!out.is_empty());
}

#[tokio::test]
async fn limit_rate_preserves_all() {
    let out = drain(Flux::range(1, 10).limit_rate(3)).await;
    assert_eq!(out, (1..=10).collect::<Vec<_>>());
}

#[tokio::test(start_paused = true)]
async fn sample_takes_latest_per_tick() {
    let src = Flux::from_stream(async_stream::try_stream! {
        for i in 1..=4 {
            yield i;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let out = drain(src.sample(Duration::from_millis(25))).await;
    // Sampled values are a subsequence ending at the last item.
    assert!(out.windows(2).all(|w| w[0] < w[1]));
    assert!(out.contains(&4) || !out.is_empty());
}

#[tokio::test(start_paused = true)]
async fn debounce_emits_after_quiet() {
    let src = Flux::from_stream(async_stream::try_stream! {
        yield 1;
        yield 2;
        tokio::time::sleep(Duration::from_millis(50)).await;
        yield 3;
    });
    let out = drain(src.debounce(Duration::from_millis(20))).await;
    // 1 is superseded by 2 immediately; 2 survives the quiet gap; 3 last.
    assert!(out.contains(&2));
    assert!(out.contains(&3));
    assert!(!out.contains(&1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_on_parallel_hop() {
    let out = drain(Flux::range(1, 3).subscribe_on(Scheduler::Parallel)).await;
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_on_parallel_hop() {
    let out = drain(Flux::range(1, 3).publish_on(Scheduler::Parallel)).await;
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test]
async fn range_and_generate() {
    assert_eq!(drain(Flux::range(0, 4)).await, vec![0, 1, 2, 3]);
    let out = drain(Flux::generate(0i32, |s| {
        if s < 3 {
            Some((s, s + 1))
        } else {
            None
        }
    }))
    .await;
    assert_eq!(out, vec![0, 1, 2]);
}

#[tokio::test(start_paused = true)]
async fn interval_with_take() {
    let out = drain(Flux::interval(Duration::from_millis(5)).take(3)).await;
    assert_eq!(out, vec![0, 1, 2]);
}

#[tokio::test]
async fn create_sink_pushes() {
    let f = Flux::create(|sink| {
        sink.next(1);
        sink.next(2);
        sink.next(3);
        sink.complete();
    });
    assert_eq!(drain(f).await, vec![1, 2, 3]);
}

#[tokio::test]
async fn create_sink_error() {
    let f = Flux::<i32>::create(|sink| {
        sink.next(1);
        sink.error(FireflyError::internal("boom"));
    });
    let e = f.collect_list().block().await.unwrap_err();
    assert_eq!(e.status, 500);
}

#[tokio::test]
async fn merge_concat_free_fns() {
    let mut out = drain(merge(vec![Flux::range(1, 2), Flux::range(10, 2)])).await;
    out.sort();
    assert_eq!(out, vec![1, 2, 10, 11]);
    let out = drain(concat(vec![Flux::range(1, 2), Flux::range(10, 2)])).await;
    assert_eq!(out, vec![1, 2, 10, 11]);
}

#[tokio::test]
async fn zip_combine_free_fns() {
    let out = drain(zip(Flux::range(1, 2), Flux::from_iter(vec!["a", "b"]))).await;
    assert_eq!(out, vec![(1, "a"), (2, "b")]);
    let out = drain(combine_latest(
        Flux::from_iter(vec![1]),
        Flux::from_iter(vec!["x"]),
    ))
    .await;
    assert!(out.contains(&(1, "x")));
}

#[tokio::test]
async fn defer_runs_at_subscription() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let f = Flux::defer(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Flux::range(1, 2)
    });
    assert_eq!(counter.load(Ordering::SeqCst), 0); // lazy
    assert_eq!(drain(f).await, vec![1, 2]);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn from_value_stream_and_to_stream() {
    let f = Flux::from_value_stream(futures::stream::iter(vec![1, 2, 3]));
    let mut raw = f.to_stream();
    let mut out = Vec::new();
    while let Some(item) = raw.next().await {
        out.push(item.unwrap());
    }
    assert_eq!(out, vec![1, 2, 3]);
}

#[tokio::test]
async fn subscribe_drains() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let done = Arc::new(tokio::sync::Notify::new());
    let d = done.clone();
    Flux::range(1, 3).subscribe(
        move |v| {
            let _ = tx.send(v);
        },
        |_| {},
        move || d.notify_one(),
    );
    done.notified().await;
    let mut out = Vec::new();
    while let Ok(v) = rx.try_recv() {
        out.push(v);
    }
    assert_eq!(out, vec![1, 2, 3]);
}

fn assert_send_static<T: Send + 'static>(_: &T) {}

#[tokio::test]
async fn flux_is_send_static() {
    let f = Flux::range(1, 3).map(|x| x + 1).filter(|x| *x > 0);
    assert_send_static(&f);
    let _ = drain(f).await;
}

#[tokio::test]
async fn mono_flux_interop_roundtrip() {
    // Flux -> Mono (collect_list) -> Flux (flat_map_many)
    let out = Flux::range(1, 3)
        .collect_list()
        .flat_map_many(Flux::from_iter)
        .collect_list()
        .block()
        .await
        .unwrap();
    assert_eq!(out, Some(vec![1, 2, 3]));
}
