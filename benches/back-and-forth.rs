use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

fn back_and_forth(n: usize) {
    use hzrd::HzrdCell;

    let cell = HzrdCell::new(None);

    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..n {
                while cell.read().is_some() {
                    std::hint::spin_loop();
                }
                cell.set(Some(i));
            }
        });

        s.spawn(|| {
            for _ in 0..n {
                while cell.read().is_none() {
                    std::hint::spin_loop();
                }
                cell.set(None);
            }
        });
    });
}

pub fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("back-and-forth", |b| {
        b.iter(|| back_and_forth(black_box(1_000_000)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
