use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering::*};
use std::sync::Barrier;
use std::time::Duration;

use hzrd::core::{Action, Domain, ReadHandle, RetiredPtr};
use hzrd::HzrdCell;

fn read_unchecked(domain: impl Domain + Send + Sync) {
    let unique_ptr = |i: i32| Box::into_raw(Box::new(i));
    let value = AtomicPtr::new(unique_ptr(-1));

    let set_value = |new_value| {
        let old_ptr = value.swap(unique_ptr(new_value), SeqCst);
        let non_null_ptr = unsafe { NonNull::new_unchecked(old_ptr) };
        domain.retire(unsafe { RetiredPtr::new(non_null_ptr) });
    };

    std::thread::scope(|s| {
        s.spawn(|| {
            let hzrd_ptr = domain.hzrd_ptr();
            while unsafe { *ReadHandle::read_unchecked(&value, hzrd_ptr, Action::Reset) } != 32 {
                std::hint::spin_loop();
            }
            set_value(-1);
        });

        for i in 0..40 {
            s.spawn(move || {
                set_value(i);
            });
        }
    });

    let _ = unsafe { Box::from_raw(value.load(SeqCst)) };
}

fn hzrd_ptrs(domain: impl Domain + Send + Sync + Copy) {
    let cell = HzrdCell::new_in(String::new(), domain);
    let barrier = Barrier::new(2);

    std::thread::scope(|s| {
        s.spawn(|| {
            barrier.wait();
            let _hzrd_ptrs: Vec<_> = (0..40).map(|_| domain.hzrd_ptr()).collect();
        });

        s.spawn(|| {
            barrier.wait();
            for _ in 0..40 {
                cell.set(String::from("Hello world"));
            }
        });
    });
}

fn read_cell(domain: impl Domain + Send + Sync) {
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
    fn read_unchecked() {
        super::read_unchecked(GlobalDomain);
    }

    #[test]
    fn hzrd_ptrs() {
        super::hzrd_ptrs(GlobalDomain);
    }

    #[test]
    fn read_cell() {
        super::read_cell(GlobalDomain);
    }

    #[test]
    fn holding_handles() {
        super::holding_handles(GlobalDomain);
    }
}

mod shared_domain {
    use hzrd::SharedDomain;

    #[test]
    fn read_unchecked() {
        super::read_unchecked(SharedDomain::new());
    }

    #[test]
    fn hzrd_ptrs() {
        super::hzrd_ptrs(&SharedDomain::new());
    }

    #[test]
    fn read_cell() {
        super::read_cell(SharedDomain::new());
    }

    #[test]
    fn holding_handles() {
        super::holding_handles(SharedDomain::new());
    }
}
