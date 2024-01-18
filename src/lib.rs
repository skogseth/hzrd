/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

# HzrdCell
The core API of this crate is the [`HzrdCell`], which provides an API reminiscent to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, an significant overhead and additional indirection.

Reading the value of the cell, e.g. via the [`get`](HzrdCell::get) method, is lock-free. Writing to the value is instant, but there is some overhead following the swap which requires locking the metadata of the cell (to allow some synchronization of garbage collection between the cells). The main way of writing to the value is via the [`set`](HzrdCell::set) method. Here is an example of [`HzrdCell`] in use.

```
use std::time::Duration;
use std::thread;

use hzrd::HzrdCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Running,
    Finished,
}

let state = HzrdCell::new(State::Idle);

thread::scope(|s| {
    s.spawn(|| {
        thread::sleep(Duration::from_millis(1));
        match state.get() {
            State::Idle => println!("Waiting is boring, ugh"),
            State::Running => println!("Let's go!"),
            State::Finished => println!("We got here too late :/"),
        }
    });

    s.spawn(|| {
        state.set(State::Running);
        thread::sleep(Duration::from_millis(1));
        state.set(State::Finished);
    });
});

assert_eq!(state.get(), State::Finished);
```
*/

mod stack;

pub mod core;

mod private {
    // We want to test the code in the readme
    //
    // TODO: Data race in the README (yeah, for real)
    #![doc = include_str!("../README.md")]
}

// ------------------------------------------

use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering::*};

use core::{Domain, HzrdPtr, RetiredPtr, SharedDomain};

pub static GLOBAL_DOMAIN: SharedDomain = SharedDomain::new();

/**
Holds a value protected by hazard pointers.

Each [`HzrdCell`] belongs to a given domain, which contains the set of hazard- and retired pointers protecting the value. See the [`Domain`](core::Domain) trait for more details on this.

See the [crate-level documentation](crate) for a "getting started" guide.
*/
pub struct HzrdCell<T, D> {
    value: AtomicPtr<T>,
    domain: D,
}

impl<T: 'static> HzrdCell<T, &'static SharedDomain> {
    /**
    Construct a new [`HzrdCell`] with the given value in the default domain: [`SharedDomain`]. See [`HzrdCell::new_in`] if you want to construct it in a custom domain.
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(0);
    #
    # let mut cell = cell;
    # assert_eq!(cell.get(), 0);
    ```
    */
    pub fn new(value: T) -> Self {
        Self::new_in(value, &GLOBAL_DOMAIN)
    }
}

impl<T: 'static, D: Domain> HzrdCell<T, D> {
    /**
    Set the value of the cell

    This method may block after the value has been set.

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(0);
    cell.set(1);
    #
    # let mut cell = cell;
    # assert_eq!(cell.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        // SAFETY: We retire the pointer in a valid domain
        let old_ptr = unsafe { self.swap(Box::new(value)) };
        self.domain.retire(old_ptr);
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, value: T) {
        // SAFETY: We retire the pointer in a valid domain
        let old_ptr = unsafe { self.swap(Box::new(value)) };
        self.domain.just_retire(old_ptr);
    }

    /**
    Get a handle holding a reference to the current value held by the `HzrdCell`

    The functionality of this is somewhat similar to a [`MutexGuard`](std::sync::MutexGuard), except the [`ReadHandle`] only accepts reading the value. There is no locking mechanism needed to grab this handle, although there might be a short wait if the read overlaps with a write.

    The [`ReadHandle`] acquired holds a shared reference to the value of the [`HzrdCell`] as it was when the [`read`](Self::read) function was called. If the value of the [`HzrdCell`] is changed after the [`ReadHandle`] is acquired its new value is not reflected in the value of the [`ReadHandle`], the old value is held alive atleast until all references are dropped. See the documentation of [`ReadHandle`] for more information.

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(String::from("Hey"));

    let handle = cell.read();

    // We can create multiple references from a single handle
    let string: &str = &*handle;
    let bytes: &[u8] = handle.as_bytes();

    assert_eq!(string, "Hey");
    assert_eq!(bytes, [72, 101, 121]);
    ```
    */
    pub fn read(&self) -> ReadHandle<'_, T> {
        // Retrieve a new hazard pointer
        let hzrd_ptr = self.domain.hzrd_ptr();

        let mut ptr = self.value.load(SeqCst);
        loop {
            // SAFETY: Non-null ptr
            unsafe { hzrd_ptr.store(ptr) };

            // We now need to keep updating it until it is in a consistent state
            ptr = self.value.load(SeqCst);
            if ptr as usize == hzrd_ptr.get() {
                break;
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = unsafe { &*ptr };
        ReadHandle { value, hzrd_ptr }
    }

    /**
    Get the value of the cell (requires the type to be [`Copy`])

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(100);
    assert_eq!(cell.get(), 100);
    ```
    */
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        *self.read()
    }

    /**
    Read the contained value and clone it (requires type to be [`Clone`])

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new([1, 2, 3]);
    assert_eq!(cell.cloned(), [1, 2, 3]);
    ```
    */
    pub fn cloned(&self) -> T
    where
        T: Clone,
    {
        self.read().clone()
    }

    /// Reclaim available memory, if possible
    pub fn reclaim(&self) {
        self.domain.reclaim();
    }
}

impl<T: 'static, D> HzrdCell<T, D> {
    /**
    Construct a new [`HzrdCell`] in the given domain.
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].

    # Example
    ```
    use std::sync::Arc;

    use hzrd::HzrdCell;
    use hzrd::core::SharedDomain;

    let custom_domain = Arc::new(SharedDomain::new());
    let cell_1 = HzrdCell::new_in(0, Arc::clone(&custom_domain));
    let cell_2 = HzrdCell::new_in(false, Arc::clone(&custom_domain));
    # assert_eq!(cell_1.get(), 0);
    # assert_eq!(cell_2.get(), false);
    ```
    */
    pub fn new_in(value: T, domain: D) -> Self {
        let value = AtomicPtr::new(Box::into_raw(Box::new(value)));
        Self { value, domain }
    }

    // SAFETY: Requires correct handling of RetiredPtr
    unsafe fn swap(&self, boxed: Box<T>) -> RetiredPtr {
        let new_ptr = Box::into_raw(boxed);

        // SAFETY: Ptr must at this point be non-null
        let old_raw_ptr = self.value.swap(new_ptr, SeqCst);
        let non_null_ptr = unsafe { NonNull::new_unchecked(old_raw_ptr) };

        // SAFETY: We can guarantee it's pointing to heap-allocated memory
        unsafe { RetiredPtr::new(non_null_ptr) }
    }
}

impl<T, D> Drop for HzrdCell<T, D> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(SeqCst)) };
    }
}

// SAFETY: Both the type held and the domain need to be `Send`
unsafe impl<T: Send, D: Send> Send for HzrdCell<T, D> {}

// SAFETY: This may be somewhat defensive?
unsafe impl<T: Send + Sync, D: Send + Sync> Sync for HzrdCell<T, D> {}

/// Holds a reference to a read value. The value is kept alive by a hazard pointer.
pub struct ReadHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr,
}

impl<T> Deref for ReadHandle<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<T> Drop for ReadHandle<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We are dropping so `value` will never be accessed after this
        unsafe { self.hzrd_ptr.free() };
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn global_domain() {
        let val_1 = HzrdCell::new(0);
        let val_2 = HzrdCell::new(false);

        let _handle_1 = val_1.read();
        val_1.set(1);

        assert_eq!(val_2.domain.retired.lock().unwrap().len(), 1);

        drop(_handle_1);
    }

    #[test]
    fn shallow_drop_test() {
        let _ = HzrdCell::new(0);
    }

    #[test]
    fn deep_drop_test() {
        let _ = HzrdCell::new(vec![1, 2, 3]);
    }

    #[test]
    fn single() {
        let string = String::new();
        let cell = HzrdCell::new(string);

        {
            let handle = cell.read();
            assert_eq!(handle.len(), 0);
            assert_eq!(*handle, "");
        }

        let new_string = String::from("Hello world!");
        cell.set(new_string);

        {
            let handle = cell.read();
            assert_eq!(handle.len(), 12);
            assert_eq!(*handle, "Hello world!");
        }

        cell.reclaim();
    }

    #[test]
    fn double() {
        let string = String::new();
        let cell = HzrdCell::new(string);

        std::thread::scope(|s| {
            s.spawn(|| {
                let handle = cell.read();
                std::thread::sleep(Duration::from_millis(200));
                assert_eq!(*handle, "");
            });

            std::thread::sleep(Duration::from_millis(100));

            s.spawn(|| {
                let handle = cell.read();
                assert_eq!(*handle, "");
                drop(handle);

                let new_string = String::from("Hello world!");
                cell.set(new_string);
            });
        });

        cell.reclaim();
        assert_eq!(cell.cloned(), "Hello world!");
    }

    /*
    #[test]
    fn manual_reclaim() {
        let cell = HzrdCell::new([1, 2, 3]);

        cell.just_set([4, 5, 6]);
        assert_eq!(cell.num_retired(), 1);

        cell.just_set([7, 8, 9]);
        assert_eq!(cell.num_retired(), 2);

        cell.reclaim();
        assert_eq!(cell.num_retired(), 0);
    }
    */
}
