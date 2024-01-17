/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

# HzrdCell
The core API of this crate is the [`HzrdCell`], which provides an API reminiscent to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, a significant overhead for creating/destroying cells, as well as some... funkiness. [`HzrdCell`] requires in contrast to the [`Mutex`](std::sync::Mutex) no additional wrapping, such as reference counting, in order to keep references valid for threads that may outlive eachother: There is an inherent reference count in the core functionality of [`HzrdCell`] which maintains this. A consequence of this is that [`HzrdCell`]s should not form cycles.

Reading the value of the cell, e.g. via the [`get`](HzrdCell::get) method, is lock-free. Writing to the value is instant, but there is some overhead following the swap which requires locking the metadata of the cell (to allow some synchronization of garbage collection between the cells). The main way of writing to the value is via the [`set`](HzrdCell::set) method. Here is an example of [`HzrdCell`] in use.

```
# fn main() -> Result<(), &'static str> {
use std::time::Duration;
use std::thread;

use hzrd::HzrdCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Running,
    Finished,
}

let mut state = HzrdCell::new(State::Idle);

let mut state_1 = HzrdCell::clone(&state);
let handle_1 = thread::spawn(move || {
    thread::sleep(Duration::from_millis(1));
    match state_1.get() {
        State::Idle => println!("Waiting is boring, ugh"),
        State::Running => println!("Let's go!"),
        State::Finished => println!("We got here too late :/"),
    }
});

let state_2 = HzrdCell::clone(&state);
let handle_2 = thread::spawn(move || {
    state_2.set(State::Running);
    thread::sleep(Duration::from_millis(1));
    state_2.set(State::Finished);
});

handle_1.join().map_err(|_| "Thread 1 failed")?;
handle_2.join().map_err(|_| "Thread 2 failed")?;

assert_eq!(state.get(), State::Finished);
#
#     Ok(())
# }
```

# The elephant in the room
The keen eye might have observed some of the "funkiness" of [`HzrdCell`] in the previous example: Reading the value of the cell required it to be mutable, whilst writing to the cell did not. Exclusivity is usually associated with mutation, but for the [`HzrdCell`] this relationship is inversed in order to bend the rules of mutation. Another example, here using the [`read`](HzrdCell::read) function to acquire a [`RefHandle`] to the underlying value, as it doesn't implement copy:

```
# use hzrd::HzrdCell;
#
// NOTE: The cell must be marked as mutable to allow reading the value
let mut cell = HzrdCell::new([0, 1, 2]);

// NOTE: Associated function syntax used to clarify mutation requirement
let handle = HzrdCell::read(&mut cell);
assert_eq!(handle[0], 0);
```
*/

mod stack;
mod utils;

pub mod core;
// pub mod pair;

mod private {
    // We want to test the code in the readme
    #![doc = include_str!("../README.md")]
}

// ------------------------------------------

use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering::*};

use core::{Domain, HzrdPtr, RetiredPtr, SharedDomain};

pub static GLOBAL_DOMAIN: SharedDomain = SharedDomain::new();

/**
Holds a value protected by hazard pointers

Each value belongs to a given domain, which contains the set of hazard- and retired pointers protecting the value.

See the [crate-level documentation](crate) for more details.
*/
pub struct HzrdCell<T, D> {
    value: AtomicPtr<T>,
    domain: D,
}

impl<T: 'static> HzrdCell<T, &'static SharedDomain> {
    /**
    Construct a new [`HzrdCell`] with the given value in the default domain: [`SharedDomain`]. See [`HzrdCell::new_in`] if you want to construct it in a custom domain.
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].
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

impl<T: 'static, D> HzrdCell<T, D> {
    /**
    Construct a new [`HzrdCell`] in the given domain.
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].
    ```
    # use hzrd::HzrdCell;
    #
    use hzrd::core::SharedDomain;
    let custom_domain = Arc::new(SharedDomain::new());
    let cell = HzrdCell::new_in(0, custom_domain);
    #
    # let mut cell = cell;
    # assert_eq!(cell.get(), 0);
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

    // SAFETY: Must be a unique hazard pointer kept valid for the lifetime of the handle
    unsafe fn read_with_hzrd_ptr<'hzrd>(&self, hzrd_ptr: &'hzrd HzrdPtr) -> RefHandle<'hzrd, T> {
        let mut ptr = self.value.load(SeqCst);
        // SAFETY: Non-null ptr
        unsafe { hzrd_ptr.store(ptr) };

        // We now need to keep updating it until it is in a consistent state
        loop {
            ptr = self.value.load(SeqCst);
            if ptr as usize == hzrd_ptr.get() {
                break;
            } else {
                // SAFETY: Non-null ptr
                unsafe { hzrd_ptr.store(ptr) };
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = unsafe { &*ptr };
        RefHandle { value, hzrd_ptr }
    }
}

impl<T: 'static, D: Domain> HzrdCell<T, D> {
    /**
    Set the value of the cell

    This method may block after the value has been set.

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

    The functionality of this is somewhat similar to a [`MutexGuard`](std::sync::MutexGuard), except the [`RefHandle`] only accepts reading the value. There is no locking mechanism needed to grab this handle, although there might be a short wait if the read overlaps with a write.

    The [`RefHandle`] acquired holds a shared reference to the value of the [`HzrdCell`] as it was when the [`read`](Self::read) function was called. If the value of the [`HzrdCell`] is changed after the [`RefHandle`] is acquired its new value is not reflected in the value of the [`RefHandle`], the old value is held alive atleast until all references are dropped. See the documentation of [`RefHandle`] for more information.

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let string = String::from("Hey");

    // NOTE: The cell must be marked as mutable to allow calling `read`
    let mut cell = HzrdCell::new(string);

    // NOTE: Associated function syntax used to clarify mutation requirement
    let handle = HzrdCell::read(&mut cell);

    // We can create multiple references from a single handle
    let string: &str = &*handle;
    let bytes: &[u8] = handle.as_bytes();

    assert_eq!(string, "Hey");
    assert_eq!(bytes, [72, 101, 121]);
    ```
    */
    pub fn read(&self) -> RefHandle<'_, T> {
        // Retrieve a new hazard pointer
        let hzrd_ptr = self.domain.hzrd_ptr();

        // SAFETY: We have a valid hazard pointer to this domain
        unsafe { self.read_with_hzrd_ptr(&hzrd_ptr) }
    }

    /**
    Get the value of the cell (requires the type to be [`Copy`])

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

impl<T, D> Drop for HzrdCell<T, D> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(SeqCst)) };
    }
}

// SAFETY: This may be somewhat defensive
unsafe impl<T: Send + Sync, D: Domain + Send + Sync> Send for HzrdCell<T, D> {}

// SAFETY: This may be somewhat defensive
unsafe impl<T: Send + Sync, D: Domain + Send + Sync> Sync for HzrdCell<T, D> {}

/// Holds a reference to an object protected by a hazard pointer
pub struct RefHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr,
}

impl<T> Deref for RefHandle<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<T> Drop for RefHandle<'_, T> {
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
