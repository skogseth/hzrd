#![warn(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
//#![warn(rustdoc::missing_doc_code_examples)]

/*!
This crate provides a safe API for shared mutability, using hazard pointers for memory reclamation.

# Hazard pointers

Hazard pointers is a strategy for controlled memory reclamation in multithreaded contexts. All readers/writers have shared access to some data, as well as a collection of garbage, and a list of hazard pointers. Whenever you read the value of the data you get a reference to it, which you also store in one of the hazard pointers. Writing to the data is done by swapping out the old value for a new one; the old value is then "retired" (thrown in the pile of garbage). Retired values are only reclaimed if no hazard pointers contain their address. In this way the hazard pointers end up protecting any references from becoming invalid.

# HzrdCell
The core API of this crate is the [`HzrdCell`], which provides an API reminiscent to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

```
use hzrd::HzrdCell;

let cell = HzrdCell::new(false);

std::thread::scope(|s| {
    s.spawn(|| {
        // Loop until the value is true
        while !cell.get() {
            std::hint::spin_loop();
        }

        // And then set it back to false!
        cell.set(false);
    });

    s.spawn(|| {
        // Set the value to true
        cell.set(true);

        // And then read the value!
        // This might print either `true` or `false`
        println!("{}", cell.get());
    });
});
```
*/

mod stack;

pub mod core;
pub mod domains;

mod private {
    // We want to test the code in the readme
    #![doc = include_str!("../README.md")]
}

// ------------------------------------------

use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering::*};

use crate::core::{Action, Domain, HzrdPtr, ReadHandle, RetiredPtr};
use crate::domains::GlobalDomain;

// -------------------------------------

/**
Holds a value protected by hazard pointers.

Each [`HzrdCell`] belongs to a given domain, which contains the set of hazard pointers protecting the value. See the [`Domain`] trait for more details on this.

See the [crate-level documentation](crate) for a "getting started" guide.
*/
pub struct HzrdCell<T, D = GlobalDomain> {
    value: AtomicPtr<T>,
    domain: D,
}

impl<T: 'static> HzrdCell<T> {
    /**
    Construct a new [`HzrdCell`] with the given value in the default domain.

    The default domain is a globally shared domain, see [`GlobalDomain`] for more information on this domain. This is the recommended way for constructing [`HzrdCell`]s, unless you really know what you're doing, in which case you can use [`HzrdCell::new_in`] to construct a new cell in a custom domain.

    # Note
    The value held in the cell will be allocated on the heap via [`Box`], and is stored seperate from the metadata associated with the [`HzrdCell`].

    # Example
    ```
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new(0);
    # assert_eq!(cell.get(), 0);
    ```
    */
    pub fn new(value: T) -> Self {
        Self::new_in(value, GlobalDomain)
    }
}

impl<T: 'static, D: Domain> HzrdCell<T, D> {
    /**
    Set the value of the cell

    This will perform the following operations (in this order):
    - Allocate the new value on the heap using [`Box`]
    - Swap out the old value for the new value
    - Retire the old value
    - Reclaim retired values, if possible

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

    /// Set the value of the cell without attempting to reclaim memory
    pub fn just_set(&self, value: T) {
        // SAFETY: We retire the pointer in a valid domain
        let old_ptr = unsafe { self.swap(Box::new(value)) };
        self.domain.just_retire(old_ptr);
    }

    /**
    Get a handle holding a reference to the current value held by the [`HzrdCell`]

    The functionality of this is somewhat similar to a [`MutexGuard`](std::sync::MutexGuard), except the [`ReadHandle`] only accepts reading the value. There is no locking mechanism needed to grab this handle, although there might be a short wait if the read overlaps with a write.

    The [`ReadHandle`] acquired holds a shared reference to the value of the [`HzrdCell`] as it was when the [`read`](Self::read) function was called. If the value of the [`HzrdCell`] is changed after the [`ReadHandle`] is acquired its new value is not reflected in the value of the [`ReadHandle`]. The hazard pointer held by the handle will keep the old value alive. See the documentation of [`ReadHandle`] for more information.

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

    Constructing a reader can be helpful (and more performant) when doing consecutive reads, as the reader will hold a [`HzrdPtr`] which will be reused for each read. The reader exposes a similar API to [`HzrdCell`], with the exception of "write-actions" such as [`HzrdCell::set`] & [`HzrdCell::reclaim`]. See [`HzrdReader`] for more details.

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

    This method is aimed at more advanced usage of this library, as it requires more knowledge about hazard pointer domains and how they work  The recommended way for most users to construct a [`HzrdCell`] is using the [`new`](`HzrdCell::new`) function, which uses a globally shared domain.

    A good starting point for using this function is to understand the basics of the [`Domain`](`core::Domain`) trait. You can then browse the various implementations of this trait provided by this crate in the [`domains`]-module.

    # Note
    The value held in the cell will be allocated on the heap via [`Box`], and is stored seperate from the metadata associated with the [`HzrdCell`].

    ```
    # use hzrd::domains::SharedDomain;
    # use hzrd::HzrdCell;
    let cell = HzrdCell::new_in(0, SharedDomain::new());
    ```
    */
    pub fn new_in(value: T, domain: D) -> Self {
        let value = AtomicPtr::new(Box::into_raw(Box::new(value)));
        Self { value, domain }
    }

    /// # SAFETY
    /// Requires correct handling of [`RetiredPtr`]
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
The keen eye might have observed some of the "funkiness" with [`HzrdReader`] in the previous example: Reading the value requires a mutable/exclusive reference, and thus the reader must be marked as `mut`. Note also that this was not the case for reading, or even writing (!), to the cell itself. Exclusivity is usually associated with mutation, but for the [`HzrdReader`] this is not the case. The reason that reading via a [`HzrdReader`] requires a mutable/exclusive reference is that the reader holds access to only a single hazard pointer. This hazard pointer is used when you read a value, so as long as you hold on the value read that hazard pointer is busy.

```
# use hzrd::{HzrdCell, HzrdReader};
#
let cell = HzrdCell::new([0, 1, 2]);

// NOTE: The reader must be marked as mutable
let mut reader = cell.reader();

// NOTE: We use associated function syntax here to
//       emphasize the need for a mutable reference
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
}

impl<T> Drop for HzrdReader<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We are the current owner of the hazard pointer
        unsafe { self.hzrd_ptr.release() };
    }
}

// SAFETY: The type held needs to be both `Send` and `Sync`
unsafe impl<T: Send + Sync> Send for HzrdReader<'_, T> {}

// SAFETY: The type held needs to be both `Send` and `Sync`
unsafe impl<T: Send + Sync> Sync for HzrdReader<'_, T> {}

// ------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::domains::{LocalDomain, SharedDomain};
    use crate::HzrdCell;

    #[test]
    fn drop_test() {
        // Shallow drop
        let _ = HzrdCell::new(0);

        // Deep drop
        let _ = HzrdCell::new(vec![1, 2, 3]);
    }

    #[test]
    fn types() {
        let _cell_1: HzrdCell<usize> = HzrdCell::new(0);
        let _cell_2: HzrdCell<_, LocalDomain> = HzrdCell::new_in(0, LocalDomain::new());

        let shared_domain = Arc::new(SharedDomain::new());
        let _cell_3: HzrdCell<_, Arc<_>> = HzrdCell::new_in(0, Arc::clone(&shared_domain));
        let _cell_4: HzrdCell<_, &Arc<SharedDomain>> = HzrdCell::new_in(0, &shared_domain);

        let _cell_4: HzrdCell<_, _> = HzrdCell::new_in(0, Box::new(SharedDomain::new()));

        let _cell_5: HzrdCell<usize, _> = HzrdCell::new_in(0, LocalDomain::new());
        let _cell_6: HzrdCell<usize, LocalDomain> = HzrdCell::new_in(0, LocalDomain::new());

        // Invalid:
        // let _cell_x: HzrdCell<_> = HzrdCell::new_in(false, Box::new(SharedDomain::new()));
    }

    #[test]
    fn single_threaded() {
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
    fn multi_threaded() {
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
        assert_eq!(cell.read().clone(), "Hello world!");
    }

    #[test]
    fn static_threads() {
        let cell = Arc::new(HzrdCell::new(Vec::new()));

        let cell_1 = Arc::clone(&cell);
        let handle_1 = std::thread::spawn(move || {
            let handle = cell_1.read();
            std::thread::sleep(Duration::from_millis(200));
            assert!(handle.is_empty());
        });

        std::thread::sleep(Duration::from_millis(100));

        let cell_2 = Arc::clone(&cell);
        let handle_2 = std::thread::spawn(move || {
            let handle = cell_2.read();
            assert!(handle.is_empty());
            drop(handle);

            let new_vec = vec![false, false, true];
            cell_2.set(new_vec);
        });

        handle_1.join().unwrap();
        handle_2.join().unwrap();

        cell.reclaim();
        assert_eq!(cell.read().as_slice(), [false, false, true]);
    }

    #[test]
    fn retirement() {
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
    fn hazard_pointers_are_reused() {
        let local_domain = LocalDomain::new();
        let cell = HzrdCell::new_in(0, &local_domain);

        assert_eq!(local_domain.number_of_hzrd_ptrs(), 0);

        assert_eq!(cell.get(), 0);
        assert_eq!(local_domain.number_of_hzrd_ptrs(), 1);

        // Should just reuse the same hazard pointer each read
        for _ in 0..10 {
            assert_eq!(cell.get(), 0);
        }
        assert_eq!(local_domain.number_of_hzrd_ptrs(), 1);

        // We should still only be using this one hazard pointer
        let reader = cell.reader();
        assert_eq!(local_domain.number_of_hzrd_ptrs(), 1);
        drop(reader);

        // Should just reuse the same hazard pointer for each reader
        for _ in 0..10 {
            let _ = cell.reader();
        }
        assert_eq!(local_domain.number_of_hzrd_ptrs(), 1);
    }
}
