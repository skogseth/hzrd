use std::sync::Barrier;
use std::time::Duration;

use hzrd::core::Domain;
use hzrd::HzrdCell;

fn read(domain: impl Domain + Send + Sync) {
    let cell = HzrdCell::new_in(String::new(), domain);
    let barrier = Barrier::new(2);

    std::thread::scope(|s| {
        s.spawn(|| {
            barrier.wait();
            for _ in 0..40 {
                let _ = cell.read();
            }
        });

        s.spawn(|| {
            barrier.wait();
            for _ in 0..40 {
                cell.set(String::from("Hello world"));
            }
        });
    });
}

fn holding_handles(domain: impl Domain + Send + Sync) {
    let cell = HzrdCell::new_in(String::from("hello"), domain);

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

mod global_domain {
    use hzrd::GlobalDomain;

    #[test]
    fn read() {
        super::read(GlobalDomain);
    }

    #[test]
    fn holding_handles() {
        super::holding_handles(GlobalDomain);
    }
}

mod shared_domain {
    use hzrd::SharedDomain;

    #[test]
    fn read() {
        super::read(SharedDomain::new());
    }

    #[test]
    fn holding_handles() {
        super::holding_handles(SharedDomain::new());
    }
}
