/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

The main entrypoint to this crate is the [`HzrdCell`], which provides an API similar to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiole threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, a significant overhead for creating/destroying cells, as well as some... funkiness. [`HzrdCell`] requires in contrast to the [`Mutex`](std::sync::Mutex) no additional wrapping, such as reference counting, in order to keep references valid for threads that may outlive eachother. There is an inherent reference count in the core functionality of [`HzrdCell`] which maintains this safety.

[`HzrdCell`] is particularly nice to work with if the underlying type implements copy. The [`get`](HzrdCell::get) method is lock-free, and requires minimal overhead. The [`set`](HzrdCell::set) method is mostly lock-free: The value is instantly updated, but there is some overhead following the swap which requires a lock to be acquired. However, this lock holds no contention with the reading methods of the cell.

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

let state = HzrdCell::new(State::Idle);

let state_1 = HzrdCell::clone(&state);
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

If you want to immutably borrow the underlying value then this is done by acquiring a [`RefHandle`]. At this point the [`HzrdCell`] shows off some of its "funkiness". Acquiring a [`RefHandle`] requires an exclusive (aka mutable) borrow of the cell, which in turn means the cell must be marked as mutable. Here is an example for a non-copy type, where a [`RefHandle`] is acquired and used.

```
# fn main() -> Result<(), &'static str> {
use hzrd::HzrdCell;

let string = String::from("Hello, world!");
let cell = HzrdCell::new(string);

// Notice the strangeness that aquiring the `RefHandle` requires mut
let mut cell_1 = HzrdCell::clone(&cell);
let handle_1 = std::thread::spawn(move || {
    let string = HzrdCell::read(&mut cell_1);
    if *string == "Hello, world!" {
        println!("I was first");
    } else {
        println!("The other thread was first");
    }
});

// ...whilst changing the value does not
let cell_2 = HzrdCell::clone(&cell);
let handle_2 = std::thread::spawn(move || {
    cell_2.set(String::new());
});
#
#     handle_1.join().map_err(|_| "Thread 1 failed")?;
#     handle_2.join().map_err(|_| "Thread 2 failed")?;
#
#     Ok(())
# }
```

There is no way to acquire a mutable borrow to the underlying value as that inherently requires locking the value.
*/

use std::fmt::Display;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

mod cell;
mod linked_list;
mod ptr;
mod utils;

use crate::cell::HzrdCellInner;
use crate::ptr::HzrdPtr;
use crate::utils::RetiredPtr;
use crate::utils::{allocate, free};

/**
Holds a value that can be shared, and mutated, across multiple threads

See the [crate-level documentation](crate) for more details.
*/
pub struct HzrdCell<T> {
    inner: NonNull<HzrdCellInner<T>>,
    hzrd_ptr: NonNull<HzrdPtr>,
    marker: PhantomData<T>,
}

// Private methods
impl<T> HzrdCell<T> {
    fn inner(&self) -> &HzrdCellInner<T> {
        // SAFETY: Only shared references to this are allowed
        unsafe { self.inner.as_ref() }
    }

    fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
}

impl<T> HzrdCell<T> {
    /**
    Construct a new [`HzrdCell`] with the given value
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].
    It is therefore recommended to use [`HzrdCell::from`] if you already have a [`Box<T>`].

    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(0);
    #
    # assert_eq!(cell.get(), 0);
    ```
    */
    pub fn new(value: T) -> Self {
        HzrdCell::from(Box::new(value))
    }

    /**
    Set the value of the cell

    This method may block after the value has been set.

    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(0);
    cell.set(1);
    #
    # assert_eq!(cell.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        let inner = self.inner();
        let old_ptr = inner.swap(value);

        let mut retired = inner.retired.lock().unwrap();
        retired.push_back(RetiredPtr::new(old_ptr));

        let Ok(hzrd_ptrs) = inner.hzrd_ptrs.try_lock() else {
            return;
        };

        crate::utils::reclaim(&mut retired, &hzrd_ptrs);
    }

    /// Replace the contained value with a new value, returning the old
    ///
    /// This will block until all [`ReadHandle`]s have been dropped
    #[doc(hidden)]
    pub fn replace(&self, value: T) -> T {
        let _ = value;
        todo!()
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
        let inner = self.inner();
        let hzrd_ptr = self.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer
        // - Value is immediately copied, and ReadHandle is dropped
        unsafe { *inner.read(hzrd_ptr) }
    }

    /**
    Get a handle to read the value held by the `HzrdCell`

    The functionality of this is somewhat similar to a [`MutexGuard`](std::sync::MutexGuard), except the [`RefHandle`] only accepts reading the value. There is no locking mechanism needed to grab this handle, although there might be a short wait if the read overlaps with a write.

    The [`RefHandle`] acquired holds a shared reference to the value of the [`HzrdCell`] as it was when the [`read`](Self::read) function was called. If the value of the [`HzrdCell`] is changed after the [`RefHandle`] is acquired its new value is not reflected in the value of the [`RefHandle`], the old value is held alive atleast until all references are dropped. See the documentation of [`RefHandle`] for more information.

    # The elephant in the room
    Acquiring a [`RefHandle`] does, maybe surprisingly, require a mutable borrow. This is caused by a core invariant of the cell: There can only be one read at any given point. This exclusivity is usually associated with mutation, but for the [`HzrdCell`] (as well as the standard library's [`Cell`](std::cell::Cell)) this relationship is inversed in order to bend the rules of mutation. To remedy some of this "strangeness" there are multiple helper functions to avoid directly relying on [`RefHandle`]s, such as [`get`](Self::get), [`cloned`](Self::cloned) and [`read_and_map`](Self::read_and_map).

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let string = String::from("Hey");

    // NOTE: The cell must be marked as mutable to allow calling `read`
    let mut cell = HzrdCell::new(string);

    // NOTE: Associated function syntax is required
    let handle = HzrdCell::read(&mut cell);

    // We can create multiple references from a single handle
    let string: &str = &*handle;
    let bytes: &[u8] = handle.as_bytes();

    assert_eq!(string, "Hey");
    assert_eq!(bytes, [72, 101, 121]);
    ```
    */
    pub fn read(cell: &mut Self) -> RefHandle<'_, T> {
        let inner = cell.inner();
        let hzrd_ptr = cell.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer
        // - ReadHandle holds exlusive reference via &mut, meaning
        //   no other accesses to hazard pointer before it is dropped
        unsafe { inner.read(hzrd_ptr) }
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
        let inner = self.inner();
        let hzrd_ptr = self.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer
        // - Value is immediately cloned, and RefHandle is dropped
        unsafe { inner.read(hzrd_ptr).clone() }
    }

    /**
    Read contained value and map it

    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new([1, 2, 3]);
    let mut vec = cell.read_and_map(|arr| arr.as_slice().to_owned());
    vec.push(4);
    assert_eq!(vec, [1, 2, 3, 4]);
    ```

    Note that the output cannot hold a reference to the input (see [`RefHandle`] for why)
    ```compile_fail
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(String::from("Hello, world!"));
    let bytes = cell.read_and_map(|s| s.as_bytes()); // <- tries to hold on to reference
    # let _ = bytes;
    ```
    */
    pub fn read_and_map<U, F: FnOnce(&T) -> U>(&self, f: F) -> U {
        let inner = self.inner();
        let hzrd_ptr = self.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer
        // - We don't access the hazard pointer for the rest of the function
        let value = unsafe { inner.read(hzrd_ptr) };

        f(&value)
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, value: T) {
        let inner = self.inner();
        let old_ptr = inner.swap(value);
        inner
            .retired
            .lock()
            .unwrap()
            .push_back(RetiredPtr::new(old_ptr));
    }

    /// Reclaim available memory
    ///
    /// This method may block
    pub fn reclaim(&self) {
        self.inner().reclaim();
    }

    /// Try to reclaim memory, but don't wait for the shared lock to do so
    pub fn try_reclaim(&self) {
        self.inner().try_reclaim();
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        self.inner().retired.lock().unwrap().len()
    }
}

unsafe impl<T: Send> Send for HzrdCell<T> {}

impl<T> Clone for HzrdCell<T> {
    fn clone(&self) -> Self {
        let hzrd_ptr = self.inner().add();

        HzrdCell {
            inner: self.inner,
            hzrd_ptr,
            marker: PhantomData,
        }
    }
}

impl<T: Display> Display for HzrdCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.read_and_map(|x| x.fmt(f))
    }
}

impl<T> From<Box<T>> for HzrdCell<T> {
    fn from(boxed: Box<T>) -> Self {
        let inner = HzrdCellInner::new(boxed);
        let hzrd_ptr = inner.add();

        HzrdCell {
            inner: allocate(inner),
            hzrd_ptr,
            marker: PhantomData,
        }
    }
}

impl<T> Drop for HzrdCell<T> {
    fn drop(&mut self) {
        // SAFETY: We scope this so that all references/pointers are dropped before inner is dropped
        let should_drop_inner = {
            // SAFETY: The HzrdPtr is exclusively owned by the current cell
            unsafe { self.hzrd_ptr().free() };

            // TODO: Handle panic?
            let hzrd_ptrs = self.inner().hzrd_ptrs.lock().unwrap();
            hzrd_ptrs.all_available()
        };

        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.inner) };
        }
    }
}

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
        unsafe { self.hzrd_ptr.clear() };
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

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
        let mut cell = HzrdCell::new(string);

        {
            let handle = HzrdCell::read(&mut cell);
            assert_eq!(handle.len(), 0);
            assert_eq!(*handle, "");
        }

        let new_string = String::from("Hello world!");
        cell.set(new_string);

        {
            let handle = HzrdCell::read(&mut cell);
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
            let mut cell_1 = HzrdCell::clone(&cell);
            s.spawn(move || {
                let handle = HzrdCell::read(&mut cell_1);
                std::thread::sleep(Duration::from_millis(200));
                assert_eq!(*handle, "");
            });

            std::thread::sleep(Duration::from_millis(100));

            let mut cell_2 = HzrdCell::clone(&cell);
            s.spawn(move || {
                let handle = HzrdCell::read(&mut cell_2);
                assert_eq!(*handle, "");
                drop(handle);

                let new_string = String::from("Hello world!");
                cell_2.set(new_string);
            });
        });

        cell.reclaim();
        assert_eq!(cell.cloned(), "Hello world!");
    }

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

    #[test]
    fn from_boxed() {
        let boxed = Box::new([1, 2, 3]);
        let cell = HzrdCell::from(boxed);
        let arr = cell.cloned();
        assert_eq!(arr, [1, 2, 3]);
        cell.set(arr.map(|x| x + 1));
        assert_eq!(cell.cloned(), [2, 3, 4]);
    }
}
