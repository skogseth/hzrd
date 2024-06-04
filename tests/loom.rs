#![cfg(loom)]

use loom::sync::atomic::{AtomicPtr, Ordering::*};
use loom::sync::Arc;

use hzrd::core::{Action, HzrdPtr, ReadHandle};
use hzrd::{HzrdCell, SharedDomain};

/*
#[test]
fn core_primitives() {
    loom::model(|| {
        let original = Box::into_raw(Box::new(0));

        let value = Box::leak(Box::new(AtomicPtr::new(original)));
        let hzrd_ptr = Box::leak(Box::new(HzrdPtr::new()));

        let handle_1 = loom::thread::spawn(|| {
            let handle = unsafe { ReadHandle::read_unchecked(value, hzrd_ptr, Action::Release) };
            assert!(matches!(*handle, 0 | 1));
        });

        let handle_2 = loom::thread::spawn(|| {
            let new = Box::into_raw(Box::new(1));
            value.swap(new, SeqCst)
        });

        handle_1.join().unwrap();

        let original = handle_2.join().unwrap();

        // SAFETY: Hazard pointer must be freed at this point
        assert_eq!(hzrd_ptr.get(), 0);
        let _ = unsafe { Box::from_raw(original) };
    });
}
*/

#[test]
fn core_primitives_0() {
    loom::model(|| {
        let original = Box::into_raw(Box::new(0));

        let value = AtomicPtr::new(original);
        let hzrd_ptr = HzrdPtr::new();

        let handle = unsafe { ReadHandle::read_unchecked(&value, &hzrd_ptr, Action::Release) };
        //assert_eq!(*handle, 0);
    });
}

#[test]
fn core_primitives_1() {
    loom::model(|| {
        let original = Box::into_raw(Box::new(0));

        let value = Arc::new(AtomicPtr::new(original));
        let hzrd_ptr = Arc::new(HzrdPtr::new());

        let handle = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                let handle =
                    unsafe { ReadHandle::read_unchecked(&*value, &*hzrd_ptr, Action::Release) };
                assert_eq!(*handle, 0);
            }
        });

        handle.join().unwrap();
    });
}

#[test]
fn core_primitives_2() {
    loom::model(|| {
        let original = Box::into_raw(Box::new(0));

        let value = Arc::new(AtomicPtr::new(original));
        let hzrd_ptr = Arc::new(HzrdPtr::new());

        let handle_1 = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                let handle =
                    unsafe { ReadHandle::read_unchecked(&*value, &*hzrd_ptr, Action::Release) };
                assert!(matches!(*handle, 0 | 1), "Value was {}", *handle);
            }
        });

        let handle_2 = loom::thread::spawn({
            let value = Arc::clone(&value);
            move || {
                let new = Box::into_raw(Box::new(1));
                value.swap(new, SeqCst)
            }
        });

        handle_1.join().unwrap();

        let original = handle_2.join().unwrap();

        // SAFETY: Hazard pointer must be freed at this point
        assert_eq!(hzrd_ptr.get(), 0);
        let _ = unsafe { Box::from_raw(original) };
    });
}

/*
#[test]
fn my_first_loom_test() {
    loom::model(|| {
        let domain = Arc::new(SharedDomain::new());

        let cell = Arc::new(HzrdCell::new_in(0, Arc::clone(&domain)));

        assert_eq!(cell.get(), 0);

        let handle_1 = loom::thread::spawn({
            let cell = Arc::clone(&cell);
            move || {
                cell.set(1);
            }
        });

        let handle_2 = loom::thread::spawn({
            let cell = Arc::clone(&cell);
            move || {
                cell.set(2);
            }
        });

        let val = cell.get();
        assert!(matches!(val, 0 | 1 | 2), "Value was {val}");

        handle_1.join().unwrap();
        handle_2.join().unwrap();

        cell.set(3);
        assert_eq!(cell.get(), 3);
    });
}
*/

#[test]
fn second_attempt() {
    loom::model(|| {
        let domain = Arc::new(SharedDomain::new());

        let cell = Arc::new(HzrdCell::new_in(0, Arc::clone(&domain)));

        assert_eq!(cell.get(), 0);

        loom::thread::spawn({
            let cell = Arc::clone(&cell);
            move || {
                cell.set(1);
            }
        });

        let val = cell.get();
        assert!(matches!(val, 0 | 1), "Value was {val}");
    });
}
