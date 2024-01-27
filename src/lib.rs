#![warn(unsafe_op_in_unsafe_fn)]
//#![warn(missing_docs)]
//#![warn(rustdoc::missing_doc_code_examples)]

/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

# HzrdCell
The core API of this crate is the [`HzrdCell`], which provides an API reminiscent to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, an significant overhead and additional indirection. Here is an example of [`HzrdCell`] in use.

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

use std::cell::UnsafeCell;
use std::collections::{BTreeSet, LinkedList};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering::SeqCst};
use std::sync::Mutex;

use crate::core::{Domain, HzrdPtr, RetiredPtr};
use crate::stack::SharedStack;

// ------------------------------------------

/// Shared, multithreaded domain
#[derive(Debug)]
pub struct SharedDomain {
    hzrd: SharedStack<HzrdPtr>,
    retired: Mutex<Vec<RetiredPtr>>,
}

impl SharedDomain {
    /**
    Construct a new, clean shared domain

    # Example
    ```
    # use hzrd::SharedDomain;
    let domain = SharedDomain::new();
    ```
    */
    pub const fn new() -> Self {
        Self {
            hzrd: SharedStack::new(),
            retired: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        self.hzrd.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        self.retired.lock().unwrap().len()
    }
}

unsafe impl Domain for SharedDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        match self.hzrd.iter().find_map(|node| node.try_acquire()) {
            Some(hzrd_ptr) => hzrd_ptr,
            None => self.hzrd.push(HzrdPtr::new()),
        }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        self.retired.lock().unwrap().push(ret_ptr);
    }

    fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken
        let Ok(mut retired_ptrs) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        let hzrd_ptrs: BTreeSet<_> = self.hzrd.iter().map(HzrdPtr::get).collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        // Grab the lock to retired pointers
        let mut retired_ptrs = self.retired.lock().unwrap();

        // And retire the given pointer
        retired_ptrs.push(ret_ptr);

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        let hzrd_ptrs: BTreeSet<_> = self.hzrd.iter().map(HzrdPtr::get).collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }
}

// -------------------------------------

use shared_cell::SharedCell;

mod shared_cell {
    pub(crate) struct SharedCell<T>(T);

    impl<T> SharedCell<T> {
        pub(crate) fn new(value: T) -> Self {
            Self(value)
        }

        pub(crate) fn get(&self) -> &T {
            &self.0
        }
    }
}

/// Local, singlethreaded domain
#[derive(Debug)]
pub struct LocalDomain {
    // Important to only allow shared references to the HzrdPtr's
    hzrd: UnsafeCell<LinkedList<SharedCell<HzrdPtr>>>,
    retired: UnsafeCell<Vec<RetiredPtr>>,
}

impl LocalDomain {
    /**
    Construct a new, clean local domain

    # Example
    ```
    # use hzrd::LocalDomain;
    let domain = LocalDomain::new();
    ```
    */
    pub const fn new() -> Self {
        Self {
            hzrd: UnsafeCell::new(LinkedList::new()),
            retired: UnsafeCell::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        unsafe { &*self.hzrd.get() }.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        unsafe { &*self.retired.get() }.len()
    }
}

unsafe impl Domain for LocalDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        {
            let hzrd_ptrs = unsafe { &*self.hzrd.get() };

            if let Some(hzrd_ptr) = hzrd_ptrs.iter().find_map(|node| node.get().try_acquire()) {
                return hzrd_ptr;
            }
        }

        let hzrd_ptrs = unsafe { &mut *self.hzrd.get() };
        hzrd_ptrs.push_back(SharedCell::new(HzrdPtr::new()));
        unsafe { hzrd_ptrs.back().unwrap_unchecked().get() }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        retired_ptrs.push(ret_ptr);
    }

    fn reclaim(&self) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        let hzrd_ptrs = unsafe { &*self.hzrd.get() };

        let hzrd_ptrs: BTreeSet<_> = hzrd_ptrs
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        let hzrd_ptrs = unsafe { &*self.hzrd.get() };

        retired_ptrs.push(ret_ptr);

        let hzrd_ptrs: BTreeSet<_> = hzrd_ptrs
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }
}

// -------------------------------------

pub static GLOBAL_DOMAIN: SharedDomain = SharedDomain::new();

/**
Holds a value protected by hazard pointers.

Each [`HzrdCell`] belongs to a given domain, which contains the set of hazard pointers protecting the value. See the [`Domain`] trait for more details on this.

See the [crate-level documentation](crate) for a "getting started" guide.

# Advanced usage
The domain can, for example, be held in the cell itself. This means the cell will hold exclusive access to it, and the garbage associated with the domain will be cleaned up when the cell is dropped. This can be abused to delay all garbage collection for some limited time in order to do it all in bulk:

```
use hzrd::{HzrdCell, SharedDomain};

let cell = HzrdCell::new_in(0, SharedDomain::new());

std::thread::scope(|s| {
    // Let's see how quickly we can count to thirty
    s.spawn(|| {
        for i in 0..30 {
            // Intentionally avoid all garbage collection
            cell.just_set(i);
        }
    });

    s.spawn(|| {
        println!("Let's check what the value is! {}", cell.get());
    });
});
```

Another option is to have the domain stored in an [`Arc`](`std::sync::Arc`). Multiple cells can now share a single domain, but that domain (including all the associated garbage) will still be guaranteed to be cleaned up when all the cells are dropped.
```
use std::sync::Arc;

use hzrd::{HzrdCell, SharedDomain};

let custom_domain = Arc::new(SharedDomain::new());
let cell_1 = HzrdCell::new_in(0, Arc::clone(&custom_domain));
let cell_2 = HzrdCell::new_in(false, Arc::clone(&custom_domain));
# assert_eq!(cell_1.get(), 0);
# assert_eq!(cell_2.get(), false);
```
*/
#[derive(Debug)]
pub struct HzrdCell<T, D> {
    value: AtomicPtr<T>,
    domain: D,
}

impl<T: 'static> HzrdCell<T, &'static SharedDomain> {
    /**
    Construct a new [`HzrdCell`] with the given value in the default domain.

    The default domain is a globally shared domain: A &'static to [`GLOBAL_DOMAIN`]. This is the recommended way for constructing [`HzrdCell`]s unless you really know what you're doing, in which case you can use [`HzrdCell::new_in`] to construct in a custom domain.

    The value held in the cell will be allocated on the heap via [`Box`], and is stored seperate from the metadata associated with the [`HzrdCell`].

    # Example
    ```
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new(0);
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
    let cell = HzrdCell::new(0);
    cell.set(1);
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

        // SAFETY: The hazard pointer will protect the value
        unsafe { ReadHandle::read_unchecked(&self.value, hzrd_ptr, Action::Release) }
    }

    /**
    Read the associated value and copy it (requires the type to be [`Copy`])

    # Example
    ```
    # use hzrd::HzrdCell;
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

    /**
    Reclaim available memory, if possible

    # Example
    ```
    # use hzrd::HzrdCell;
    #
    let cell = HzrdCell::new(0);

    cell.just_set(1); // Current garbage: [0]
    cell.just_set(2); // Current garbage: [0, 1]
    cell.reclaim(); // Current garbage: []
    ```
    */
    pub fn reclaim(&self) {
        self.domain.reclaim();
    }

    /**
    Construct a reader to the current cell

    Constructing a reader can be helpful (and more performant) when doing consecutive reads.
    The reader will hold a [`HzrdPtr`] which will be reused for each read. The reader exposes
    a similar API to [`HzrdCell`], with the exception of "write-action" such as
    [`HzrdCell::set`] & [`HzrdCell::reclaim`]. See [`HzrdReader for more details`].

    # Example
    ```
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new(false);
    let reader = cell.reader();
    # let mut reader = reader;
    # assert_eq!(reader.get(), false)
    ```
    */
    pub fn reader(&self) -> HzrdReader<'_, T> {
        HzrdReader {
            value: &self.value,
            hzrd_ptr: self.domain.hzrd_ptr(),
        }
    }
}

impl<T: 'static, D> HzrdCell<T, D> {
    /**
    Construct a new [`HzrdCell`] in the given domain.

    The recommended way for most users to construct a [`HzrdCell`] is using the [`new`](`HzrdCell::new`) function, which uses a global, shared domain. This method is aimed at more advanced usage of this library.

    The value held in the cell will be allocated on the heap via [`Box`], and is stored seperate from the metadata associated with the [`HzrdCell`].

    ```
    # use hzrd::{HzrdCell, SharedDomain};
    let cell = HzrdCell::new_in(0, SharedDomain::new());
    ```
    */
    pub fn new_in(value: T, domain: D) -> Self {
        let value = AtomicPtr::new(Box::into_raw(Box::new(value)));
        Self { value, domain }
    }

    /// SAFETY: Requires correct handling of RetiredPtr
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

// ------------------------------

#[derive(Debug, Clone, Copy)]
enum Action {
    Reset,
    Release,
}

/**
Holds a reference to a read value. The value is kept alive by a hazard pointer.

Note that the reference held by the handle is to the value as it was when it was read.
If the cell is written to during the lifetime of the handle this will not be reflected in its value.

# Example
```
# use hzrd::HzrdCell;
let cell = HzrdCell::new(vec![1, 2, 3, 4]);

// Read the value and hold on to a reference to the value
let handle = cell.read();
assert_eq!(handle[..], [1, 2, 3, 4]);

// NOTE: The value is not updated when the cell is written to
cell.set(Vec::new());
assert_eq!(handle[..], [1, 2, 3, 4]);
```
*/
#[derive(Debug)]
pub struct ReadHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr,
    action: Action,
}

impl<'hzrd, T> ReadHandle<'hzrd, T> {
    unsafe fn read_unchecked(
        value: &'hzrd AtomicPtr<T>,
        hzrd_ptr: &'hzrd HzrdPtr,
        action: Action,
    ) -> Self {
        let mut ptr = value.load(SeqCst);
        loop {
            // SAFETY: Non-null ptr
            unsafe { hzrd_ptr.protect(ptr) };

            // We now need to keep updating it until it is in a consistent state
            let new_ptr = value.load(SeqCst);
            if ptr == new_ptr {
                break;
            } else {
                ptr = new_ptr;
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = unsafe { &*ptr };

        Self {
            value,
            hzrd_ptr,
            action,
        }
    }
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
        match self.action {
            Action::Reset => unsafe { self.hzrd_ptr.reset() },
            Action::Release => unsafe { self.hzrd_ptr.release() },
        }
    }
}

// ------------------------------

/**
A reader object for a specific [`HzrdCell`]

The [`HzrdReader`] holds a reference to the value of the [`HzrdCell`], as well as a [`HzrdPtr`] to read from it. When performing many, consecutive reads of a cell this can be much more performant, as you only need to retrieve the [`HzrdPtr`] once.

# Basic usage
The basics of using a [`HzrdReader`] is to first create a [`HzrdCell`], and then call the [`reader`](`HzrdCell::reader`) method on the cell to retrieve a reader.

```
# use std::time::Duration;
#
# use hzrd::HzrdCell;
#
let cell = HzrdCell::new(false);

std::thread::scope(|s| {
    s.spawn(|| {
        let mut reader = cell.reader();
        while !reader.get() {
            std::hint::spin_loop();
        }
        println!("Done!");
    });

    s.spawn(|| {
        std::thread::sleep(Duration::from_millis(1));
        cell.set(true);
    });
});
```

# The elephant in the room
The keen eye might have observed some of the "funkiness" with [`HzrdReader`] in the previous example: Reading the value of from the reader required it to be mutable, whilst reading/writing to the cell did not. Exclusivity is usually associated with mutation, but for the [`HzrdReader`] this is not the case. The reason that reading via a [`HzrdReader`] require an exclusive/mutable reference is that the returned [`ReadHandle`] requires exclusive access to the internal [`HzrdPtr`].

Another example, here using the [`read`](HzrdReader::read) function to acquire a [`ReadHandle`] to the underlying value, as it doesn't implement copy:

```
# use hzrd::{HzrdCell, HzrdReader};
#
let cell = HzrdCell::new([0, 1, 2]);

// NOTE: The reader must be marked as mutable
let mut reader = cell.reader();

// NOTE: Associated function syntax used to make the requirement explicit
let handle = HzrdReader::read(&mut reader);
assert_eq!(handle[0], 0);
```
*/
pub struct HzrdReader<'cell, T> {
    value: &'cell AtomicPtr<T>,
    hzrd_ptr: &'cell HzrdPtr,
}

impl<T> HzrdReader<'_, T> {
    /**
    Read the associated value and return a handle holding a reference it

    Note that the reference held by the returned handle is to the value as it was when it was read.
    If the cell is written to during the lifetime of the handle this will not be reflected in its value.

    # Example
    ```
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new(String::new());
    let mut reader = cell.reader();
    let string = reader.read();
    assert!(string.is_empty());
    ```
    */
    pub fn read(&mut self) -> ReadHandle<'_, T> {
        // SAFETY: The hazard pointer will protect the value
        unsafe { ReadHandle::read_unchecked(self.value, self.hzrd_ptr, Action::Reset) }
    }

    /**
    Read the associated value and copy it (requires the type to be [`Copy`])

    # Example
    ```
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new('z');
    let mut reader = cell.reader();
    assert_eq!(reader.get(), 'z');
    ```
    */
    pub fn get(&mut self) -> T
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
    let cell = HzrdCell::new(vec![1, 2, 3]);
    let mut reader = cell.reader();
    assert_eq!(reader.cloned(), [1, 2, 3]);
    ```
    */
    pub fn cloned(&mut self) -> T
    where
        T: Clone,
    {
        self.read().clone()
    }
}

// SAFETY: The type held needs to be both `Send` and `Sync`
unsafe impl<T: Send + Sync> Send for HzrdReader<'_, T> {}

// SAFETY: The type held needs to be both `Send` and `Sync`
unsafe impl<T: Send + Sync> Sync for HzrdReader<'_, T> {}

// ------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::time::Duration;

    use super::*;

    fn new_value<T>(value: T) -> NonNull<T> {
        let boxed = Box::new(value);
        let raw = Box::into_raw(boxed);
        unsafe { NonNull::new_unchecked(raw) }
    }

    #[test]
    fn shared_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = SharedDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let set: BTreeSet<_> = domain.hzrd.iter().map(HzrdPtr::get).collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }

    #[test]
    fn local_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = LocalDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let set: BTreeSet<_> = unsafe { &*domain.hzrd.get() }
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }

    #[test]
    fn cell() {
        let val_1 = HzrdCell::new(0);
        let val_2 = HzrdCell::new(false);

        let _handle_1 = val_1.read();
        val_1.set(1);

        assert_eq!(val_2.domain.number_of_retired_ptrs(), 1);

        drop(_handle_1);
    }

    #[test]
    fn drop_test() {
        // Shallow drop
        let _ = HzrdCell::new(0);

        // Deep drop
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

    #[test]
    fn cell_and_retirement() {
        let cell = HzrdCell::new_in(String::new(), SharedDomain::new());
        assert_eq!(cell.domain.number_of_hzrd_ptrs(), 0, "{:?}", cell.domain);

        let _handle_1 = cell.read();
        assert_eq!(cell.domain.number_of_hzrd_ptrs(), 1, "{:?}", cell.domain);

        cell.set("Hello world".into());
        assert_eq!(cell.domain.number_of_retired_ptrs(), 1);

        // ------------

        let _handle_2 = cell.read();
        assert_eq!(cell.domain.number_of_hzrd_ptrs(), 2, "{:?}", cell.domain);

        cell.set("Pizza world".into());
        assert_eq!(cell.domain.number_of_retired_ptrs(), 2);

        // ------------

        drop(_handle_2);
        cell.set("Ramen world".into());
        assert_eq!(cell.domain.number_of_retired_ptrs(), 1);

        // ------------

        drop(_handle_1);
        cell.reclaim();
        assert_eq!(cell.domain.number_of_retired_ptrs(), 0);
    }

    #[test]
    fn readers() {
        let cell = HzrdCell::new(vec![0, 1, 2]);

        let readers: Vec<_> = (0..10).map(|_| cell.reader()).collect();

        for mut reader in readers {
            assert_eq!(reader.read().as_slice(), [0, 1, 2]);
        }
    }

    #[test]
    fn manual_reclaim() {
        let local_domain = SharedDomain::new();
        let cell = HzrdCell::new_in([1, 2, 3], local_domain);

        cell.just_set([4, 5, 6]);
        assert_eq!(
            cell.domain.number_of_retired_ptrs(),
            1,
            "Retired ptrs: {:?}",
            cell.domain,
        );

        cell.just_set([7, 8, 9]);
        assert_eq!(
            cell.domain.number_of_retired_ptrs(),
            2,
            "Retired ptrs: {:?}",
            cell.domain,
        );

        cell.reclaim();
        assert_eq!(
            cell.domain.number_of_retired_ptrs(),
            0,
            "Retired ptrs: {:?}",
            cell.domain,
        );
    }

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
    fn read_unchecked() {
        let cell = HzrdCell::new_in(0, SharedDomain::new());

        std::thread::scope(|s| {
            s.spawn(|| {
                let value = &cell.value;
                let hzrd_ptr = cell.domain.hzrd_ptr();
                while unsafe { *ReadHandle::read_unchecked(value, hzrd_ptr, Action::Release) } != 32
                {
                    std::hint::spin_loop();
                }
                cell.set(-1);
            });

            for i in 0..40 {
                let cell = &cell;
                s.spawn(move || {
                    cell.set(i);
                });
            }
        });
    }

    #[test]
    fn stress_hzrd_ptr() {
        let cell = HzrdCell::new(String::new());
        let barrier = Barrier::new(2);

        std::thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                let _hzrd_ptrs: Vec<_> = (0..40).map(|_| cell.domain.hzrd_ptr()).collect();
            });

            s.spawn(|| {
                barrier.wait();
                for _ in 0..40 {
                    cell.set(String::from("Hello world"));
                }
            });
        });
    }

    #[test]
    fn stress_test_read() {
        let cell = HzrdCell::new(String::new());
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
}
