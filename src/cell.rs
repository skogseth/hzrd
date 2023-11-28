use std::ptr::NonNull;

use crate::core::{HzrdPtr, HzrdValue, RefHandle, SharedDomain};
use crate::utils::{allocate, free};

/**
Holds a value that can be shared, and mutated, across multiple threads

See the [crate-level documentation](crate) for more details.
*/
pub struct HzrdCell<T: 'static> {
    value: NonNull<HzrdValue<T, SharedDomain>>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

// Private methods
impl<T: 'static> HzrdCell<T> {
    fn value(&self) -> &HzrdValue<T, SharedDomain> {
        // SAFETY: Only shared references to this are allowed
        unsafe { self.value.as_ref() }
    }

    fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
}

impl<T: 'static> HzrdCell<T> {
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
    pub fn new(val: T) -> Self {
        HzrdCell::from(Box::new(val))
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
    pub fn set(&self, val: T) {
        let boxed = Box::new(val);
        self.value().set(boxed);
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, val: T) {
        let boxed = Box::new(val);
        self.value().just_set(boxed);
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
        let hzrd_ptr = self.hzrd_ptr();
        unsafe { self.value().read(hzrd_ptr) }
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
        *self.read()
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
        self.read().clone()
    }

    /// Reclaim available memory, if possible
    pub fn reclaim(&self) {
        self.value().reclaim();
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        self.value().domain().retired.lock().unwrap().len()
    }
}

unsafe impl<T: Send + Sync> Send for HzrdCell<T> {}
unsafe impl<T: Send + Sync> Sync for HzrdCell<T> {}

impl<T: 'static> Clone for HzrdCell<T> {
    fn clone(&self) -> Self {
        let hzrd_ptr = self.value().hzrd_ptr();

        HzrdCell {
            value: self.value,
            hzrd_ptr,
        }
    }
}

impl<T: 'static> From<Box<T>> for HzrdCell<T> {
    fn from(boxed: Box<T>) -> Self {
        let domain = SharedDomain::new();
        let value = HzrdValue::new_in(boxed, domain);
        let hzrd_ptr = value.hzrd_ptr();

        HzrdCell {
            value: allocate(value),
            hzrd_ptr,
        }
    }
}

impl<T: 'static> Drop for HzrdCell<T> {
    fn drop(&mut self) {
        // SAFETY: The HzrdPtr is exclusively owned by the current cell
        unsafe { self.hzrd_ptr().free() };

        // SAFETY: Important to avoid a reference in `should_drop_inner`
        let should_drop_inner: bool = {
            let domain = self.value().domain();

            // We need to check if all hzrd pointers are freed
            // We lock the list of retired pointers to effectively lock this section
            match domain.retired.lock() {
                // SAFETY: Important that the guard object is named to hold onto the lock during the call
                Ok(_guard) => domain.hzrd.all_available(),

                // If the lock has been poisoned we can't know if it's safe to drop.
                // It's better to leak the data in that case.
                Err(_) => false,
            }
        };
        
        // SAFETY: Important that all references to inner are dropped before this
        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.value) };
        }
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
