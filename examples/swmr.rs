use std::time::Duration;

use hzrd::domains::LocalDomain;
use hzrd::HzrdCell;

fn main() {
    let cell = HzrdCell::new_in(0, LocalDomain::new());

    std::thread::scope(|s| {
        let mut reader = cell.reader();
        s.spawn(move || {
            while reader.get() == 0 {
                std::hint::spin_loop();
            }
        });

        let mut reader = cell.reader();
        s.spawn(move || {
            while reader.get() < 10 {
                std::hint::spin_loop();
            }
        });

        std::thread::sleep(Duration::from_millis(10));

        for i in 0..=10 {
            cell.set(i);
        }
    });
}
