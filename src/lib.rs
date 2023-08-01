use std::fmt::Display;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

mod core;
mod linked_list;
mod utils;

use crate::core::HzrdCellInner;
use crate::linked_list::Node;
use crate::utils::{allocate, free};
use crate::utils::{HzrdPtr, RetiredPtr};

/**
Holds a value that can be shared, and mutated, by multiple threads

The main advantage of [`HzrdCell`] is that reading and writing to the value is lock-free.
This is offset by increased memory use, a significant overhead for creating/destroying cells,
as well as some... funkiness.

[`HzrdCell`] is particularly nice to work with if the underlying type implements copy.
The [`get`](HzrdCell::set) method is lock-free, and requires minimal overhead.
The [`set`](HzrdCell::set) method is mostly lock-free: The value is instantly updated,
but there is some overhead following the swap which requires a lock to be acquired.
But, as stated above, this lock holds no contention with the reading methods of the cell.

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

If you want to immutably borrow the underlying value then this is done by acquiring a [`ReadHandle`]. At this point the [`HzrdCell`] shows off some of its "funkiness". Acquiring a [`ReadHandle`] requires an exlusive (aka mutable) borrow to the cell, which in turn means the cell must be marked as mutable. Here is an example for a non-copy type, where a [`ReadHandle`] is acquired and used.

```
# fn main() -> Result<(), &'static str> {
use hzrd::HzrdCell;

let string = String::from("Hello, world!");
let cell = HzrdCell::new(string);

// Notice the strangeness that aquiring the `ReadHandle` requires mut
let mut cell_1 = HzrdCell::clone(&cell);
let handle_1 = std::thread::spawn(move || {
    let string = cell_1.read_handle();
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
pub struct HzrdCell<T> {
    inner: NonNull<HzrdCellInner<T>>,
    node_ptr: NonNull<Node<HzrdPtr<T>>>,
    marker: PhantomData<T>,
}

// Private methods
impl<T> HzrdCell<T> {
    fn core(&self) -> &HzrdCellInner<T> {
        // SAFETY: Only shared references to this are allowed
        unsafe { self.inner.as_ref() }
    }

    fn hzrd_ptr(&self) -> &HzrdPtr<T> {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { &*Node::get_from_ptr(self.node_ptr) }
    }
}

impl<T> HzrdCell<T> {
    /**
    Construct a new [`HzrdCell`] with the given value
    The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].
    It is therefore recommended to use [`HzrdCell::from`] if you already have a [`Box<T>`].

    ```
    use hzrd::HzrdCell;

    let cell = HzrdCell::new(0);
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
    use hzrd::HzrdCell;

    let cell = HzrdCell::new(0);
    cell.set(1);
    assert_eq!(cell.get(), 1);
    ```
    */
    pub fn set(&self, value: T) {
        let core = self.core();
        let old_ptr = core.swap(value);

        let mut retired = core.retired.lock().unwrap();
        retired.push_back(RetiredPtr::new(old_ptr));

        let Ok(hzrd_ptrs) = core.hzrd_ptrs.try_lock() else {
            return;
        };

        core::reclaim(&mut retired, &hzrd_ptrs);
    }

    /// Replace the contained value with a new value, returning the old
    ///
    /// This will block until all [`ReadHandle`]s have been dropped
    #[doc(hidden)]
    pub fn replace(&self, value: T) -> T {
        let _ = value;
        todo!()
    }

    /// Get the value of the cell (requires the type to be [`Copy`])
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        let core = self.core();
        let hzrd_ptr = self.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer 
        // - Value is immediately copied, and ReadHandle is dropped
        unsafe { *core.read(hzrd_ptr) }
    }

    /// Get a handle to read the value held by the `HzrdCell`
    ///
    /// The functionality of this is somewhat similar to [`std::sync::MutexGuard`],
    /// except the [`ReadHandle`] only accepts reading the value.
    /// There is no locking mechanism needed to grab this handle, although there
    /// might be a short wait if the read overlaps with a write.
    pub fn read_handle(&mut self) -> ReadHandle<'_, T> {
        let core = self.core();
        let hzrd_ptr = self.hzrd_ptr();
        
        // SAFETY:
        // - We are the owner of the hazard pointer 
        // - ReadHandle holds exlusive reference via &mut, meaning
        //   no other accesses to hazard pointer before it is dropped
        unsafe { core.read(hzrd_ptr) }
    }

    /// Read contained value and map it
    pub fn read_and_map<U, F: FnOnce(&T) -> U>(&self, f: F) -> U {
        let core = self.core();
        let hzrd_ptr = self.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer 
        // - We don't access the hazard pointer for the rest of the function
        let value = unsafe { core.read(hzrd_ptr) };

        f(&value)
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, value: T) {
        let core = self.core();
        let old_ptr = core.swap(value);
        core.retired.lock().unwrap().push_back(RetiredPtr::new(old_ptr));
    }

    /// Reclaim available memory
    ///
    /// This method may block
    pub fn reclaim(&self) {
        self.core().reclaim();
    }

    /// Try to reclaim memory, but don't wait for the shared lock to do so
    pub fn try_reclaim(&self) {
        self.core().try_reclaim();
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        self.core().retired.lock().unwrap().len()
    }
}

unsafe impl<T: Send> Send for HzrdCell<T> {}

impl<T> Clone for HzrdCell<T> {
    fn clone(&self) -> Self {
        // SAFETY: We can always get a shared reference to this
        let core = unsafe { self.inner.as_ref() };
        let node_ptr = core.add();

        HzrdCell {
            inner: self.inner,
            node_ptr,
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
        let (core, node_ptr) = HzrdCellInner::new(boxed);
        HzrdCell {
            inner: allocate(core),
            node_ptr,
            marker: PhantomData,
        }
    }
}

impl<T> Drop for HzrdCell<T> {
    fn drop(&mut self) {
        // SAFETY: We scope this so that all references/pointers are dropped before inner is dropped
        let should_drop_inner = {
            // SAFETY: We can always get a shared reference to this
            let core = unsafe { self.inner.as_ref() };

            // TODO: Handle panic?
            let mut hzrd_ptrs = core.hzrd_ptrs.lock().unwrap();

            // SAFETY: The node ptr is guaranteed to be a valid pointer to an element in the list
            let _ = unsafe { hzrd_ptrs.remove_node(self.node_ptr) };

            hzrd_ptrs.is_empty()
        };

        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.inner) };
        }
    }
}

pub struct ReadHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr<T>,
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
            let handle: ReadHandle<_> = cell.read_handle();
            assert_eq!(handle.len(), 0);
            assert_eq!(*handle, "");
        }

        let new_string = String::from("Hello world!");
        cell.set(new_string);

        {
            let handle: ReadHandle<_> = cell.read_handle();
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
                let handle = cell_1.read_handle();
                std::thread::sleep(Duration::from_millis(200));
                assert_eq!(*handle, "");
            });

            std::thread::sleep(Duration::from_millis(100));

            let mut cell_2 = HzrdCell::clone(&cell);
            s.spawn(move || {
                let handle = cell_2.read_handle();
                assert_eq!(*handle, "");
                drop(handle);

                let new_string = String::from("Hello world!");
                cell_2.set(new_string);
            });
        });

        cell.reclaim();
        assert_eq!(*cell.read_handle(), "Hello world!");
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
        let arr = *cell.read_handle();
        assert_eq!(arr, [1, 2, 3]);
        cell.set(arr.map(|x| x + 1));
        assert_eq!(*cell.read_handle(), [2, 3, 4]);
    }
}
