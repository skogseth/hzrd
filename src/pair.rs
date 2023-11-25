/*!
This module contains the [`HzrdWriter`]/[`HzrdReader`] pair.

This pair is the most primitive constructs found in this crate, as they contain no locking for synchronization. A consequence of this is that there is no garbage collection of the containers themselves. [`HzrdReader`] therefore holds a reference to it's [`HzrdWriter`], and is only valid for the lifetime of [`HzrdWriter`]. This makes the [`HzrdWriter`]/[`HzrdReader`] pair impractical in many situations. But! They are very good where they shine! In particular, they work excellently together with scoped threads:

```
# use std::time::Duration;
# use hzrd::pair::HzrdWriter;

let ready_writer = HzrdWriter::new(false);

std::thread::scope(|s| {
    let mut ready_reader = ready_writer.new_reader();
    s.spawn(move || {
        while !ready_reader.get() {
            std::hint::spin_loop();
        }
    });

    std::thread::sleep(Duration::from_millis(10));
    ready_writer.set(true);
});
```
*/

use std::cell::UnsafeCell;
use std::ptr::NonNull;

use crate::core::{Domain, HzrdPtr, HzrdPtrs, HzrdValue, RetiredPtr, RetiredPtrs};
use crate::RefHandle;

struct Ptrs {
    hzrd: HzrdPtrs,
    retired: RetiredPtrs,
}

impl Ptrs {
    pub const fn new() -> Self {
        Self {
            hzrd: HzrdPtrs::new(),
            retired: RetiredPtrs::new(),
        }
    }
}

/// This domain assumes that any access to the domain is unique and valid
struct UnsafeDomain(UnsafeCell<Ptrs>);

impl UnsafeDomain {
    /// Simply constructing this is unsafe, as the type unsafely implements domain
    ///
    /// # Safety
    /// Creating this requires that all uses of the object as a domain are safe and valid
    const unsafe fn new() -> Self {
        Self(UnsafeCell::new(Ptrs::new()))
    }
}

unsafe impl Domain for UnsafeDomain {
    fn hzrd_ptr(&self) -> NonNull<HzrdPtr> {
        let ptrs = unsafe { &mut *self.0.get() };
        ptrs.hzrd.get()
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        let ptrs = unsafe { &mut *self.0.get() };
        ptrs.retired.add(ret_ptr);
    }

    fn reclaim(&self) {
        let ptrs = unsafe { &mut *self.0.get() };
        ptrs.retired.reclaim(&ptrs.hzrd);
    }

    // Override this for performance?
    fn retire(&self, ret_ptr: RetiredPtr) {
        let ptrs = unsafe { &mut *self.0.get() };
        ptrs.retired.add(ret_ptr);
        ptrs.retired.reclaim(&ptrs.hzrd);
    }
}

/**
Container type with the ability to write to the contained value

For in-depth guide see the [module-level documentation](crate::pair).
*/
pub struct HzrdWriter<T> {
    core: Box<HzrdValue<T, UnsafeDomain>>,
}

impl<T> HzrdWriter<T> {
    /**
    Construct a new [`HzrdWriter`] containing the given value.
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdWriter`].
    It is therefore recommended to use [`HzrdWriter::from`] if you already have a [`Box<T>`].

    ```
    # use hzrd::pair::HzrdWriter;
    #
    let writer = HzrdWriter::new(0);
    #
    # assert_eq!(writer.new_reader().get(), 0);
    ```
    */
    pub fn new(value: T) -> Self {
        Self::from(Box::new(value))
    }

    /**
    Construct a new [`HzrdReader`] for reading the value contained by the given [`HzrdWriter`]

    ```
    # use hzrd::pair::HzrdWriter;
    #
    let writer = HzrdWriter::new(0);
    let mut reader = writer.new_reader();
    writer.set(1);
    assert_eq!(reader.get(), 1);
    ```
    */
    pub fn new_reader(&self) -> HzrdReader<T> {
        let hzrd_ptr = self.core.hzrd_ptr();
        HzrdReader {
            core: &self.core,
            hzrd_ptr,
        }
    }

    /**
    Set the value of the container

    ```
    # use hzrd::pair::HzrdWriter;
    #
    let writer = HzrdWriter::new(0);
    let mut reader = writer.new_reader();
    writer.set(1);
    assert_eq!(reader.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        self.core.set(Box::new(value));
    }
}

impl<T> From<Box<T>> for HzrdWriter<T> {
    fn from(boxed: Box<T>) -> Self {
        let domain = unsafe { UnsafeDomain::new() };
        let core = Box::new(HzrdValue::new_in(boxed, domain));
        Self { core }
    }
}

// SAFETY: We good?
unsafe impl<T: Send + Sync> Send for HzrdWriter<T> {}

/**
Container type with the ability to read the contained value.

For in-depth guide see the [module-level documentation](crate::pair).
*/
pub struct HzrdReader<'writer, T> {
    core: &'writer HzrdValue<T, UnsafeDomain>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

impl<T> HzrdReader<'_, T> {
    /**
    Construct a reader for the value contained by the given writer
    */
    pub fn from_writer(writer: &HzrdWriter<T>) -> HzrdReader<T> {
        writer.new_reader()
    }

    /**
    Get a handle holding a reference to the current value of the container

    See [`crate::cell::HzrdCell::read`] for a more detailed description
    */
    pub fn read(&mut self) -> RefHandle<T> {
        // SAFETY: Shared references allowed for entire lifetime of sel
        let hzrd_ptr = unsafe { self.hzrd_ptr.as_ref() };

        // SAFETY: We hold a mutable reference, so the hzrd-ptr can not be reused until handle is dropped
        unsafe { self.core.read(hzrd_ptr) }
    }

    /// Get the value of the container (requires the type to be [`Copy`])
    pub fn get(&mut self) -> T
    where
        T: Copy,
    {
        *self.read()
    }

    /// Read the contained value and clone it (requires type to be [`Clone`])
    pub fn cloned(&mut self) -> T
    where
        T: Clone,
    {
        self.read().clone()
    }
}

impl<T> Drop for HzrdReader<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We won't touch this after this point
        unsafe { self.hzrd_ptr.as_ref().free() };
    }
}

// SAFETY: We good?
unsafe impl<T: Send + Sync> Send for HzrdReader<'_, T> {}
unsafe impl<T: Send + Sync> Sync for HzrdReader<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_drop_test() {
        let _ = HzrdWriter::new(String::from("Hello"));
    }

    #[test]
    fn writer_moved() {
        let writer = HzrdWriter::new('a');

        std::thread::spawn(move || {
            let val: char = writer.new_reader().get();
            assert_eq!(val, 'a');
        });
    }

    #[test]
    fn from_boxed() {
        let boxed = Box::new([1, 2, 3]);
        let _ = HzrdWriter::from(boxed);
    }

    #[test]
    fn fancy() {
        let writer = HzrdWriter::new(0);

        std::thread::scope(|s| {
            let mut reader = writer.new_reader();
            s.spawn(move || {
                let handle = reader.read();
                assert!(matches!(*handle, 0 | 1));
            });

            let mut reader = writer.new_reader();
            s.spawn(move || {
                let handle = reader.read();
                assert!(matches!(*handle, 0 | 1));
            });

            writer.set(1);

            let mut reader = writer.new_reader();
            s.spawn(move || {
                let handle = reader.read();
                assert_eq!(*handle, 1);
            });
        });
    }
}
