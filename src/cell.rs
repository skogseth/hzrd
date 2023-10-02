use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard, RwLock, TryLockError};

use crate::core::{HzrdCore, HzrdPtr, HzrdPtrs, RefHandle, RetiredPtr, RetiredPtrs};
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

impl<T> crate::core::Read for HzrdCell<T> {
    type T = T;

    unsafe fn core(&self) -> &HzrdCore<Self::T> {
        &self.inner().core
    }

    unsafe fn hzrd_ptr(&self) -> &HzrdPtr {
        self.hzrd_ptr()
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
    # let mut cell = cell;
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
    # let mut cell = cell;
    # assert_eq!(cell.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        let inner = self.inner();
        let old_ptr = inner.core.swap(value);

        let mut retired = inner.retired.lock().unwrap();
        retired.add(RetiredPtr::new(old_ptr));

        let Ok(hzrd_ptrs) = inner.hzrd_ptrs.try_read() else {
            return;
        };

        retired.reclaim(&hzrd_ptrs);
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, value: T) {
        let inner = self.inner();
        let old_ptr = inner.core.swap(value);
        inner.retired.lock().unwrap().add(RetiredPtr::new(old_ptr));
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
    pub fn read(&mut self) -> RefHandle<T> {
        <Self as crate::core::Read>::read(self)
    }

    /**
    Get the value of the cell (requires the type to be [`Copy`])

    ```
    # use hzrd::HzrdCell;
    #
    let mut cell = HzrdCell::new(100);
    assert_eq!(cell.get(), 100);
    ```
    */
    pub fn get(&mut self) -> T
    where
        T: Copy,
    {
        <Self as crate::core::Read>::get(self)
    }

    /**
    Read the contained value and clone it (requires type to be [`Clone`])

    ```
    # use hzrd::HzrdCell;
    #
    let mut cell = HzrdCell::new([1, 2, 3]);
    assert_eq!(cell.cloned(), [1, 2, 3]);
    ```
    */
    pub fn cloned(&mut self) -> T
    where
        T: Clone,
    {
        <Self as crate::core::Read>::cloned(self)
    }

    /// Reclaim available memory, if possible
    pub fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken
        let Ok(mut retired) = self.inner().retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired.is_empty() {
            return;
        }

        // Try to access the hazard pointers
        let Ok(hzrd_ptrs) = self.inner().hzrd_ptrs.try_read() else {
            return;
        };

        retired.reclaim(&hzrd_ptrs);
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        self.inner().retired.lock().unwrap().len()
    }
}

unsafe impl<T: Send> Send for HzrdCell<T> {}
unsafe impl<T: Sync> Sync for HzrdCell<T> {}

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
        // SAFETY: The HzrdPtr is exclusively owned by the current cell
        unsafe { self.hzrd_ptr().free() };

        // SAFETY: Important that all references/pointers are dropped before inner is dropped
        let should_drop_inner = match self.inner().hzrd_ptrs.try_read() {
            // If we can read we need to check if all hzrd pointers are freed
            Ok(hzrd_ptrs) => hzrd_ptrs.all_available(),

            // If the lock would be blocked then it means someone is writing
            // ergo we are not the last HzrdCell to be dropped.
            Err(TryLockError::WouldBlock) => false,

            // If the lock has been poisoned we can't know if it's safe to drop.
            // It's better to leak the data in that case.
            Err(TryLockError::Poisoned(_)) => false,
        };

        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.inner) };
        }
    }
}

/**
Provides shared, mutable state with lock-free reads & locked writes
*/
pub struct HzrdLock<T> {
    inner: NonNull<HzrdCellInner<T>>,
    hzrd_ptr: NonNull<HzrdPtr>,
    marker: PhantomData<T>,
}

// Private methods
impl<T> HzrdLock<T> {
    fn inner(&self) -> &HzrdCellInner<T> {
        // SAFETY: Only shared references to this are allowed
        unsafe { self.inner.as_ref() }
    }

    fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
}

impl<T> crate::core::Read for HzrdLock<T> {
    type T = T;

    unsafe fn core(&self) -> &HzrdCore<Self::T> {
        &self.inner().core
    }

    unsafe fn hzrd_ptr(&self) -> &HzrdPtr {
        self.hzrd_ptr()
    }
}

impl<T> HzrdLock<T> {
    /**
    Construct a new [`HzrdLock`] with the given value
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdLock`].
    It is therefore recommended to use [`HzrdLock::from`] if you already have a [`Box<T>`].

    ```
    # use hzrd::HzrdLock;
    #
    let cell = HzrdLock::new(0);
    #
    # let mut cell = cell;
    # assert_eq!(cell.get(), 0);
    ```
    */
    pub fn new(value: T) -> Self {
        HzrdLock::from(Box::new(value))
    }

    /**
    Lock the inner value for writing (reads are not blocked)

    It the lock is not available this will block.

    ```
    # use hzrd::HzrdLock;
    #
    let mut cell_1 = HzrdLock::new(0);
    let mut cell_2 = HzrdLock::clone(&cell_1);

    // Lock the cell, other cells would now be blocked if they call `lock()`
    let mut guard = cell_1.lock();
    guard.set(10);

    // We can still read the value from other cells, only writing is locked
    assert_eq!(cell_2.get(), 10);

    // We can also read from the lock
    let val = guard.get();

    // Which can be used to update the value based on its current state
    guard.set(val + 1);
    ```
    */
    pub fn lock(&mut self) -> LockGuard<'_, T> {
        let inner = self.inner();
        LockGuard {
            core: &inner.core,
            hzrd_ptr: self.hzrd_ptr(),
            hzrd_ptrs: &inner.hzrd_ptrs,
            retired: inner.retired.lock().unwrap(),
        }
    }

    /**
    Get a handle holding a reference to the current value held by the `HzrdLock`

    The functionality of this is somewhat similar to a [`MutexGuard`](std::sync::MutexGuard), except the [`RefHandle`] only accepts reading the value. There is no locking mechanism needed to grab this handle, although there might be a short wait if the read overlaps with a write.

    The [`RefHandle`] acquired holds a shared reference to the value of the [`HzrdLock`] as it was when the [`read`](Self::read) function was called. If the value of the [`HzrdLock`] is changed after the [`RefHandle`] is acquired its new value is not reflected in the value of the [`RefHandle`], the old value is held alive atleast until all references are dropped. See the documentation of [`RefHandle`] for more information.

    # Example
    ```
    # use hzrd::HzrdLock;
    #
    let string = String::from("Hey");

    // NOTE: The cell must be marked as mutable to allow calling `read`
    let mut cell = HzrdLock::new(string);

    // NOTE: Associated function syntax used to clarify mutation requirement
    let handle = HzrdLock::read(&mut cell);

    // We can create multiple references from a single handle
    let string: &str = &*handle;
    let bytes: &[u8] = handle.as_bytes();

    assert_eq!(string, "Hey");
    assert_eq!(bytes, [72, 101, 121]);
    ```
    */
    pub fn read(&mut self) -> RefHandle<T> {
        <Self as crate::core::Read>::read(self)
    }

    /**
    Get the value of the cell (requires the type to be [`Copy`])

    ```
    # use hzrd::HzrdLock;
    #
    let mut cell = HzrdLock::new(100);
    assert_eq!(cell.get(), 100);
    ```
    */
    pub fn get(&mut self) -> T
    where
        T: Copy,
    {
        <Self as crate::core::Read>::get(self)
    }

    /**
    Read the contained value and clone it (requires type to be [`Clone`])

    ```
    # use hzrd::HzrdLock;
    #
    let mut cell = HzrdLock::new([1, 2, 3]);
    assert_eq!(cell.cloned(), [1, 2, 3]);
    ```
    */
    pub fn cloned(&mut self) -> T
    where
        T: Clone,
    {
        <Self as crate::core::Read>::cloned(self)
    }
}

unsafe impl<T: Send> Send for HzrdLock<T> {}
unsafe impl<T: Sync> Sync for HzrdLock<T> {}

impl<T> Clone for HzrdLock<T> {
    fn clone(&self) -> Self {
        let hzrd_ptr = self.inner().add();

        HzrdLock {
            inner: self.inner,
            hzrd_ptr,
            marker: PhantomData,
        }
    }
}

impl<T> From<Box<T>> for HzrdLock<T> {
    fn from(boxed: Box<T>) -> Self {
        let inner = HzrdCellInner::new(boxed);
        let hzrd_ptr = inner.add();

        HzrdLock {
            inner: allocate(inner),
            hzrd_ptr,
            marker: PhantomData,
        }
    }
}

impl<T> Drop for HzrdLock<T> {
    fn drop(&mut self) {
        // SAFETY: The HzrdPtr is exclusively owned by the current cell
        unsafe { self.hzrd_ptr().free() };

        // SAFETY: Important that all references/pointers are dropped before inner is dropped
        let should_drop_inner = match self.inner().hzrd_ptrs.try_read() {
            // If we can read we need to check if all hzrd pointers are freed
            Ok(hzrd_ptrs) => hzrd_ptrs.all_available(),

            // If the lock would be blocked then it means someone is writing
            // ergo we are not the last HzrdLock to be dropped.
            Err(TryLockError::WouldBlock) => false,

            // If the lock has been poisoned we can't know if it's safe to drop.
            // It's better to leak the data in that case.
            Err(TryLockError::Poisoned(_)) => false,
        };

        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.inner) };
        }
    }
}

pub struct LockGuard<'hzrd, T> {
    core: &'hzrd HzrdCore<T>,
    hzrd_ptr: &'hzrd HzrdPtr,
    hzrd_ptrs: &'hzrd RwLock<HzrdPtrs>,
    retired: MutexGuard<'hzrd, RetiredPtrs<T>>,
}

impl<T> crate::core::Read for LockGuard<'_, T> {
    type T = T;

    unsafe fn core(&self) -> &HzrdCore<Self::T> {
        self.core
    }

    unsafe fn hzrd_ptr(&self) -> &HzrdPtr {
        self.hzrd_ptr
    }
}

impl<T> LockGuard<'_, T> {
    pub fn set(&mut self, value: T) {
        let old_ptr = self.core.swap(value);
        self.retired.add(RetiredPtr::new(old_ptr));

        let Ok(hzrd_ptrs) = self.hzrd_ptrs.try_read() else {
            return;
        };

        self.retired.reclaim(&hzrd_ptrs);
    }

    pub fn just_set(&mut self, value: T) {
        let old_ptr = self.core.swap(value);
        self.retired.add(RetiredPtr::new(old_ptr));
    }

    pub fn read(&mut self) -> RefHandle<T> {
        <Self as crate::core::Read>::read(self)
    }

    pub fn get(&mut self) -> T
    where
        T: Copy,
    {
        <Self as crate::core::Read>::get(self)
    }

    pub fn cloned(&mut self) -> T
    where
        T: Clone,
    {
        <Self as crate::core::Read>::cloned(self)
    }

    pub fn reclaim(&mut self) {
        // Check if it's empty, no need to move forward otherwise
        if self.retired.is_empty() {
            return;
        }

        // Try to access the hazard pointers
        let Ok(hzrd_ptrs) = self.hzrd_ptrs.try_read() else {
            return;
        };

        self.retired.reclaim(&hzrd_ptrs);
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        self.retired.len()
    }
}

/// Shared heap allocated object for `HzrdCell`
///
/// The `hzrd_ptrs` keep track of pointers that are in use, and cannot be freed
/// There is one node per `HzrdCell`, which means the list also keeps track
/// of the number of active `HzrdCell`s (akin to a very inefficent atomic counter).
struct HzrdCellInner<T> {
    core: HzrdCore<T>,
    hzrd_ptrs: RwLock<HzrdPtrs>,
    retired: Mutex<RetiredPtrs<T>>,
}

impl<T> HzrdCellInner<T> {
    pub fn new(boxed: Box<T>) -> Self {
        Self {
            core: HzrdCore::new(boxed),
            hzrd_ptrs: RwLock::new(HzrdPtrs::new()),
            retired: Mutex::new(RetiredPtrs::new()),
        }
    }

    pub fn add(&self) -> NonNull<HzrdPtr> {
        self.hzrd_ptrs.write().unwrap().get()
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
        let mut cell = HzrdCell::new(string);

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
        let mut cell = HzrdCell::from(boxed);
        let arr = cell.cloned();
        assert_eq!(arr, [1, 2, 3]);
        cell.set(arr.map(|x| x + 1));
        assert_eq!(cell.cloned(), [2, 3, 4]);
    }
}
