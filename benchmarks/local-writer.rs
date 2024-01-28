use std::sync::Barrier;

use hzrd::{HzrdCell, LocalDomain};

const N: usize = 1_000_000;

fn main() {
    let cell = HzrdCell::new_in(0, LocalDomain::new());
    let barrier = Barrier::new(2);

    std::thread::scope(|s| {
        let mut reader = cell.reader();
        let barrier = &barrier;
        s.spawn(move || {
            barrier.wait();
            for _ in 0..N {
                let _ = reader.get();
            }
        });

        barrier.wait();
        for i in 1..N {
            cell.set(i);
        }
    });
}
