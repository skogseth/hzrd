/*!
This module contains the [`HzrdWriter`]/[`HzrdReader`] pair.

This pair is the most primitive constructs found in this crate, as they contain no locking for synchronization. A consequence of this is that there is no garbage collection of the containers themselves. [`HzrdReader`] therefore holds a reference to it's [`HzrdWriter`], and is only valid for the lifetime of [`HzrdWriter`]. This makes the
[`HzrdWriter`]/[`HzrdReader`] pair impractical in many situations. But! They are very good where they shine! In particular, they work excellently together with scoped threads:

```
# use std::time::Duration;
# use hzrd::pair::HzrdWriter;

let ready_writer = HzrdWriter::new(false);

std::thread::scope(|s| {
    let ready_reader = ready_writer.new_reader();
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

use std::ptr::NonNull;

use crate::core::{HzrdCore, HzrdPtr, HzrdPtrs, RetiredPtr, RetiredPtrs};
use crate::RefHandle;

/**
Container type with the ability to write to the contained value

For in-depth guide see the [module-level documentation](crate::pair).
*/
pub struct HzrdWriter<T> {
    core: Box<HzrdCore<T>>,
    ptrs: NonNull<Ptrs<T>>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

struct Ptrs<T> {
    hzrd: HzrdPtrs,
    retired: RetiredPtrs<T>,
}

unsafe impl<T> crate::core::Read for HzrdWriter<T> {
    type T = T;

    unsafe fn core(&self) -> &HzrdCore<Self::T> {
        &self.core
    }

    unsafe fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
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
    # assert_eq!(writer.get(), 0);
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
    let reader = writer.new_reader();
    writer.set(1);
    assert_eq!(reader.get(), 1);
    ```
    */
    pub fn new_reader(&self) -> HzrdReader<T> {
        // SAFETY:
        // - Only the writer can access this
        // - Writer is not Sync, so only this thread can write
        // - This thread is currently doing this
        // - The reference is not held alive beyond this function
        let ptrs = unsafe { &mut *self.ptrs.as_ptr() };

        HzrdReader {
            core: &self.core,
            hzrd_ptr: ptrs.hzrd.get(),
        }
    }

    /**
    Set the value of the container

    ```
    # use hzrd::pair::HzrdWriter;
    #
    let writer = HzrdWriter::new(0);
    let reader = writer.new_reader();
    writer.set(1);
    assert_eq!(reader.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        let old_ptr = self.core.swap(value);

        // SAFETY:
        // - Only the writer can access this
        // - Writer is not Sync, so only this thread can write
        // - This thread is currently doing this
        // - The reference is not held alive beyond this function
        let ptrs = unsafe { &mut *self.ptrs.as_ptr() };

        ptrs.retired.add(RetiredPtr::new(old_ptr));
        ptrs.retired.reclaim(&ptrs.hzrd);
    }

    /**
    Get a handle holding a reference to the current value of the container

    See [`HzrdCell::read`] for a more detailed description
    */
    pub fn read(&mut self) -> RefHandle<T> {
        <Self as crate::core::Read>::read(self)
    }

    /// Get the value of the container (requires the type to be [`Copy`])
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        <Self as crate::core::Read>::get(self)
    }

    /// Read the contained value and clone it (requires type to be [`Clone`])
    pub fn cloned(&self) -> T
    where
        T: Clone,
    {
        <Self as crate::core::Read>::cloned(self)
    }

    /// Read the contained value and map it
    ///
    /// See [`HzrdCell::read_and_map`] for a more detailed description
    pub fn read_and_map<U, F: FnOnce(&T) -> U>(&self, f: F) -> U {
        <Self as crate::core::Read>::read_and_map(self, f)
    }
}

impl<T> From<Box<T>> for HzrdWriter<T> {
    fn from(boxed: Box<T>) -> Self {
        let mut ptrs = Ptrs {
            hzrd: HzrdPtrs::new(),
            retired: RetiredPtrs::new(),
        };

        let hzrd_ptr = ptrs.hzrd.get();

        Self {
            core: Box::new(HzrdCore::new(boxed)),
            ptrs: crate::utils::allocate(ptrs),
            hzrd_ptr,
        }
    }
}

impl<T> Drop for HzrdWriter<T> {
    fn drop(&mut self) {
        // SAFETY: Noone can access this anymore
        unsafe { crate::utils::free(self.ptrs) }
    }
}

// SAFETY: We good?
unsafe impl<T> Send for HzrdWriter<T> {}

/**
Container type with the ability to read the contained value.

For in-depth guide see the [module-level documentation](crate::pair).
*/
pub struct HzrdReader<'writer, T> {
    core: &'writer HzrdCore<T>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

unsafe impl<T> crate::core::Read for HzrdReader<'_, T> {
    type T = T;

    unsafe fn core(&self) -> &HzrdCore<Self::T> {
        self.core
    }

    unsafe fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
}

impl<T> HzrdReader<'_, T> {
    /**
    Get a handle holding a reference to the current value of the container

    See [`HzrdCell::read`] for a more detailed description
    */
    pub fn read(&mut self) -> RefHandle<T> {
        <Self as crate::core::Read>::read(self)
    }

    /// Get the value of the container (requires the type to be [`Copy`])
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        <Self as crate::core::Read>::get(self)
    }

    /// Read the contained value and clone it (requires type to be [`Clone`])
    pub fn cloned(&self) -> T
    where
        T: Clone,
    {
        <Self as crate::core::Read>::cloned(self)
    }

    /// Read the contained value and map it
    ///
    /// See [`HzrdCell::read_and_map`] for a more detailed description
    pub fn read_and_map<U, F: FnOnce(&T) -> U>(&self, f: F) -> U {
        <Self as crate::core::Read>::read_and_map(self, f)
    }
}

impl<T> Drop for HzrdReader<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We won't touch this after this point
        unsafe { self.hzrd_ptr.as_ref().free() };
    }
}

// SAFETY: We good?
unsafe impl<T> Send for HzrdReader<'_, T> {}

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
                let handle = HzrdReader::read(&mut reader);
                assert!(matches!(*handle, 0 | 1));
            });

            let mut reader = writer.new_reader();
            s.spawn(move || {
                let handle = HzrdReader::read(&mut reader);
                assert!(matches!(*handle, 0 | 1));
            });

            writer.set(1);

            let mut reader = writer.new_reader();
            s.spawn(move || {
                let handle = HzrdReader::read(&mut reader);
                assert_eq!(*handle, 1);
            });
        });
    }
}
