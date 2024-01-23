use std::time::Duration;

use hzrd::core::SharedDomain;
use hzrd::HzrdCell;

#[test]
fn simple_test() {
    let cell = HzrdCell::new_in(String::from("hello"), SharedDomain::new());

    std::thread::scope(|s| {
        s.spawn(|| {
            while *cell.read() != "32" {
                std::hint::spin_loop();
            }
            cell.set(String::from("world"));
        });

        for string in (0..40).map(|i| i.to_string()) {
            s.spawn(|| cell.set(string));
        }
    });
}

#[test]
fn holding_handles() {
    let cell = HzrdCell::new_in(String::from("hello"), SharedDomain::new());

    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(1));
            let _handles: Vec<_> = (0..40).map(|_| cell.read()).collect();
        });

        for string in (0..40).map(|i| i.to_string()) {
            s.spawn(|| cell.set(string));
        }
    });
}
