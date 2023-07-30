use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

mod linked_list;
mod utils;

use crate::linked_list::{LinkedList, Node};
use crate::utils::{allocate, free};

/// Holds some address that is currently used (may be null)
type HzrdPtr<T> = AtomicPtr<T>;

/** 
Holds a value that can be shared, and mutated, by multiple threads

Provides lock-free get and set methods to the underlying value.
The downside is more excessive memory use, and locking is required for
memory reclamation as well as clone and drop.

One "funky" thing about [`HzrdCell`] is that exclusivity swaps from write to read.
This means that reading the value of the [`HzrdCell`] requires an exclusive borrow `&mut cell`, 
which means the variable must be marked as mutable.
Writing, on the other hand, only takes a shared borrow `&cell`, and so does not
require the variable to be marked as mutable. This inversion is a manifestation of the deeper
trickery that [`HzrdCell`] uses to grant lock-free, shared mutation.

# Examples
```
# fn main() -> Result<(), &'static str> {
use hzrd::HzrdCell;

let ch = 'a';
let mut cell = HzrdCell::new(ch);

// Notice the strangeness that `get` requires mut
let mut cell_1 = HzrdCell::clone(&cell);
let handle_1 = std::thread::spawn(move || {
    let read_handle = cell_1.get();
    if *read_handle == 'a' {
        println!("I was first");
    } else {
        println!("The other thread was first");
    }
});

// ...whilst `set` does not
let cell_2 = HzrdCell::clone(&cell);
let handle_2 = std::thread::spawn(move || {
    cell_2.set('!');
});

handle_1.join().map_err(|_| "Thread 1 failed")?;
handle_2.join().map_err(|_| "Thread 2 failed")?;

// If both threads have finished then the value must be '!'
assert_eq!(*cell.get(), '!');
#
#     Ok(())
# }
```
*/
pub struct HzrdCell<T> {
    inner: NonNull<HzrdCellInner<T>>,
    node_ptr: NonNull<Node<HzrdPtr<T>>>,
    marker: PhantomData<T>,
}

impl<T> HzrdCell<T> {
    /// Construct a new [`HzrdCell`] with the given value
    ///
    /// The value will be allocated on the heap seperate of the metadata associated with the [`HzrdCell`].
    /// It is therefore recommended to use [`HzrdCell::from`] if you already have a [`Box<T>`].
    pub fn new(value: T) -> Self {
        HzrdCell::from(Box::new(value))
    }

    /// Get a handle to read the value held by the `HzrdCell`
    ///
    /// The functionality of this is somewhat similar to [`std::sync::MutexGuard`],
    /// except the [`HzrdCellHandle`] only accepts reading the value.
    /// There is no locking mechanism needed to grab this handle, although there
    /// might be a short wait if the read overlaps with a write.
    pub fn get(&mut self) -> HzrdCellHandle<'_, T> {
        // SAFETY: These are only ever grabbed as shared
        let core = unsafe { self.inner.as_ref() };

        // SAFETY: We have exclusive access to this hazard ptr
        let ptr_to_hazard_ptr = unsafe { Node::get_from_ptr(self.node_ptr) };

        // SAFETY: Same as above
        let hazard_ptr = unsafe { &*ptr_to_hazard_ptr };

        let mut ptr = core.value.load(Ordering::SeqCst);
        hazard_ptr.store(ptr, Ordering::SeqCst);

        // Now need to keep updating it until consistent state
        loop {
            ptr = core.value.load(Ordering::SeqCst);
            if std::ptr::eq(ptr, hazard_ptr.load(Ordering::SeqCst)) {
                break;
            } else {
                hazard_ptr.store(ptr, Ordering::SeqCst);
            }
        }

        HzrdCellHandle {
            // SAFETY: Pointer is now held valid by the hazard ptr
            reference: unsafe { &*ptr },

            // SAFETY: Always non-null
            hazard_ptr: unsafe { NonNull::new_unchecked(ptr_to_hazard_ptr) },
        }
    }

    /// Set the value of the cell
    ///
    /// This method may block after the value has been set.
    pub fn set(&self, value: T) {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        let old_ptr = core.swap(value);

        let mut retired = core.retired.lock().unwrap();
        retired.push_back(RetiredPtr(old_ptr));

        let Ok(hazard_ptrs) = core.hazard_ptrs.try_lock() else {
            return;
        };

        HzrdCellInner::__reclaim(&mut retired, &hazard_ptrs);
    }

    /// Set the value of the cell, without reclaiming memory
    ///
    /// This method may block after the value has been set.
    pub fn just_set(&self, value: T) {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        let old_ptr = core.swap(value);

        core.retired.lock().unwrap().push_back(RetiredPtr(old_ptr));
    }

    /// Reclaim available memory
    ///
    /// This method may block
    pub fn reclaim(&self) {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        core.reclaim();
    }

    /// Try to reclaim memory, but don't wait for the shared lock to do so
    pub fn try_reclaim(&self) {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        core.try_reclaim();
    }

    /// Get the number of retired values (aka unfreed memory)
    ///
    /// This method may block
    pub fn num_retired(&self) -> usize {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        core.retired.lock().unwrap().len()
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
            let mut hazard_ptrs = core.hazard_ptrs.lock().unwrap();

            // SAFETY: The node ptr is guaranteed to be a valid pointer to an element in the list
            let _ = unsafe { hazard_ptrs.remove_node(self.node_ptr) };

            hazard_ptrs.is_empty()
        };

        if should_drop_inner {
            // SAFETY:
            // - All other cells have been dropped
            // - No pointers are held to the object
            unsafe { free(self.inner) };
        }
    }
}

pub struct HzrdCellHandle<'cell, T> {
    reference: &'cell T,
    hazard_ptr: NonNull<HzrdPtr<T>>,
}

impl<T> Deref for HzrdCellHandle<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.reference
    }
}

impl<T> Drop for HzrdCellHandle<'_, T> {
    fn drop(&mut self) {
        // SAFETY:
        // - Only shared references are valid
        // - Pointer is held alive by lifetime 'cell
        let hazard_ptr = unsafe { self.hazard_ptr.as_ref() };
        hazard_ptr.store(std::ptr::null_mut(), Ordering::SeqCst);
    }
}

struct RetiredPtr<T>(NonNull<T>);

impl<T> Drop for RetiredPtr<T> {
    fn drop(&mut self) {
        // SAFETY: No reference to this when dropped
        unsafe { free(self.0) };
    }
}

/// Shared heap allocated object for `HzrdCell`
///
/// The `hazard_ptrs` keep track of pointers that are in use, and cannot be freed
/// There is one node per `HzrdCell`, which means the list also keeps track
/// of the number of active `HzrdCell`s (akin to a very inefficent atomic counter).
struct HzrdCellInner<T> {
    value: AtomicPtr<T>,
    hazard_ptrs: Mutex<LinkedList<HzrdPtr<T>>>,
    retired: Mutex<LinkedList<RetiredPtr<T>>>,
}

impl<T> HzrdCellInner<T> {
    pub fn new(boxed: Box<T>) -> (Self, NonNull<Node<HzrdPtr<T>>>) {
        let hazard_ptr = HzrdPtr::new(std::ptr::null_mut());
        let ptr = Box::into_raw(boxed);

        let list = LinkedList::single(hazard_ptr);
        // SAFETY: There must be a head node at this point
        let node_ptr = unsafe { list.head_node().unwrap_unchecked() };

        let core = Self {
            value: AtomicPtr::new(ptr),
            hazard_ptrs: Mutex::new(list),
            retired: Mutex::new(LinkedList::new()),
        };

        (core, node_ptr)
    }

    pub fn add(&self) -> NonNull<Node<HzrdPtr<T>>> {
        let mut guard = self.hazard_ptrs.lock().unwrap();
        let hazard_ptr = HzrdPtr::new(std::ptr::null_mut());
        guard.push_back(hazard_ptr);
        // SAFETY: There must be a tail node at this point
        unsafe { guard.tail_node().unwrap_unchecked() }
    }

    fn swap(&self, value: T) -> NonNull<T> {
        let new_ptr = Box::into_raw(Box::new(value));

        // SAFETY: Ptr must at this point be non-null
        let old_raw_ptr = self.value.swap(new_ptr, Ordering::SeqCst);
        unsafe { NonNull::new_unchecked(old_raw_ptr) }
    }

    fn __reclaim(retired: &mut LinkedList<RetiredPtr<T>>, hazard_ptrs: &LinkedList<HzrdPtr<T>>) {
        let mut still_active = LinkedList::new();
        'outer: while let Some(node) = retired.pop_front() {
            for hazard_ptr in hazard_ptrs.iter() {
                if std::ptr::eq(node.0.as_ptr(), hazard_ptr.load(Ordering::SeqCst)) {
                    still_active.push_back(node);
                    continue 'outer;
                }
            }
        }

        *retired = still_active;
    }

    /// Reclaim available memory
    pub fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken, as this
        // means someone else is already reclaiming memory!
        let Ok(mut retired) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired.is_empty() {
            return;
        }

        // Wait for access to the hazard pointers
        let hazard_ptrs = self.hazard_ptrs.lock().unwrap();

        HzrdCellInner::__reclaim(&mut retired, &hazard_ptrs);
    }

    /// Try to reclaim memory, but don't wait for the shared lock to do so
    pub fn try_reclaim(&self) {
        // Try to aquire lock, exit if it is taken, as this
        // means someone else is already reclaiming memory!
        let Ok(mut retired) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired.is_empty() {
            return;
        }

        // Check if the hazard pointers are available, if not exit
        let Ok(hazard_ptrs) = self.hazard_ptrs.try_lock() else {
            return;
        };

        HzrdCellInner::__reclaim(&mut retired, &hazard_ptrs);
    }
}

impl<T> Drop for HzrdCellInner<T> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(Ordering::SeqCst)) };
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
            let handle: HzrdCellHandle<_> = cell.get();
            assert_eq!(handle.len(), 0);
            assert_eq!(*handle, "");
        }

        let new_string = String::from("Hello world!");
        cell.set(new_string);

        {
            let handle: HzrdCellHandle<_> = cell.get();
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
                let handle = cell_1.get();
                std::thread::sleep(Duration::from_millis(200));
                assert_eq!(*handle, "");
            });

            std::thread::sleep(Duration::from_millis(100));

            let mut cell_2 = HzrdCell::clone(&cell);
            s.spawn(move || {
                let handle = cell_2.get();
                assert_eq!(*handle, "");
                drop(handle);

                let new_string = String::from("Hello world!");
                cell_2.set(new_string);
            });
        });

        cell.reclaim();
        assert_eq!(*cell.get(), "Hello world!");
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
        let arr = *cell.get();
        assert_eq!(arr, [1, 2, 3]);
        cell.set(arr.map(|x| x + 1));
        assert_eq!(*cell.get(), [2, 3, 4]);
    }
}
