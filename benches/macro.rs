use std::sync::Barrier;

use hzrd::domains::LocalDomain;
use hzrd::HzrdCell;

fn back_and_forth(n: usize) {
    let cell = HzrdCell::new(None);
    let barrier = Barrier::new(2);

    std::thread::scope(|s| {
        s.spawn(|| {
            barrier.wait();
            for i in 0..n {
                while cell.read().is_some() {
                    std::hint::spin_loop();
                }
                cell.set(Some(i));
            }
        });

        s.spawn(|| {
            barrier.wait();
            for _ in 0..n {
                while cell.read().is_none() {
                    std::hint::spin_loop();
                }
                cell.set(None);
            }
        });
    });
}

fn local_writer(n: usize) {
    let cell = HzrdCell::new_in(0, LocalDomain::new());
    let barrier = Barrier::new(2);

    std::thread::scope(|s| {
        let mut reader = cell.reader();
        let barrier = &barrier;
        s.spawn(move || {
            barrier.wait();
            for _ in 0..n {
                let _ = reader.get();
            }
        });

        barrier.wait();
        for i in 1..n {
            cell.set(i);
        }
    });
}

// -------------------------------------

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

pub fn hzrd_cell(c: &mut Criterion) {
    c.bench_function("back-and-forth", |b| {
        b.iter(|| back_and_forth(black_box(1_000)))
    });

    c.bench_function("local-writer", |b| {
        b.iter(|| local_writer(black_box(1_000)))
    });
}

criterion_group!(benches, hzrd_cell);
criterion_main!(benches);
