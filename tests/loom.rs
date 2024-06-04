#![cfg(loom)]

use std::ptr::NonNull;

use loom::sync::atomic::{AtomicPtr, Ordering::*};
use loom::sync::Arc;

use hzrd::core::{Action, Domain, HzrdPtr, ReadHandle, RetiredPtr};
use hzrd::{HzrdCell, LocalDomain, SharedDomain};

#[test]
fn core_primitives() {
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
fn hazard_pointers() {
    loom::model(|| {
        let original = Box::into_raw(Box::new(0));

        let value = Arc::new(AtomicPtr::new(original));
        let hzrd_ptr = Arc::new(HzrdPtr::new());

        let handle_1 = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                let ptr = value.load(SeqCst);
                println!("Thread 1 - value: {ptr:?}");

                unsafe { hzrd_ptr.protect(ptr) };
                println!("Thread 1 - hazard pointer: {:?}", &*hzrd_ptr);

                let new_val = value.load(SeqCst);
                println!("Thread 1 - updated value: {new_val:?}");

                if new_val as usize == hzrd_ptr.get() {
                    unsafe { assert_eq!(*new_val, 0) };
                } else {
                    assert_eq!(new_val as usize, 0);
                }
            }
        });

        let handle_2 = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                println!("Thread 2 - old value: {:?}", value.load(SeqCst));
                let old = value.swap(std::ptr::null_mut(), SeqCst);
                println!("Thread 2 - new value: {:?}", value.load(SeqCst));

                let hzrd_ptr = hzrd_ptr.get();
                println!("Thread 2 - hazard pointer: {hzrd_ptr:#x}");

                if old as usize != hzrd_ptr {
                    unsafe { *old = 1 };
                }
            }
        });

        handle_1.join().unwrap();
        handle_2.join().unwrap();

        println!("");
    });
}

#[test]
fn just_local_domain() {
    loom::model(|| {
        let domain: &'static _ = Box::leak(Box::new(LocalDomain::new()));

        let original = Box::into_raw(Box::new(0));

        let value = Arc::new(AtomicPtr::new(original));
        let hzrd_ptr = Arc::new(domain.hzrd_ptr());

        let handle_1 = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                let handle =
                    unsafe { ReadHandle::read_unchecked(&*value, &*hzrd_ptr, Action::Release) };
                assert!(domain.is_protecting(hzrd_ptr.get()));
                println!("Hazard pointer value when protected: {:?}", &*hzrd_ptr);
                assert!(matches!(*handle, 0 | 1), "Value was {}", *handle);
            }
        });

        let handle_2 = loom::thread::spawn({
            let value = Arc::clone(&value);
            let hzrd_ptr = Arc::clone(&hzrd_ptr);
            move || {
                let new = Box::into_raw(Box::new(1));
                println!("Swapping value",);
                let old = value.swap(new, SeqCst);
                println!("Old pointer (before retirement): {old:?}",);
                println!("Hazard pointer (before retirement): {:?}", &*hzrd_ptr);
                let old = unsafe { RetiredPtr::new(NonNull::new_unchecked(old)) };
                domain.just_retire(old);
                let reclaimed = domain.reclaim();
                println!("Reclaimed: {reclaimed} (hazard pointer: {:?})", &*hzrd_ptr);
            }
        });

        handle_1.join().unwrap();
        handle_2.join().unwrap();

        domain.reclaim();

        println!("");
    });
}

#[test]
fn hzrd_cell_with_local_domain() {
    loom::model(|| {
        let cell = Box::leak(Box::new(HzrdCell::new_in(0, LocalDomain::new())));
        assert_eq!(cell.get(), 0);

        loom::thread::spawn(|| {
            cell.set(1);
        });

        let val = cell.get();
        assert!(matches!(val, 0 | 1), "Value was {val}");
    });
}

#[test]
fn hzrd_cell_with_shared_domain() {
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
