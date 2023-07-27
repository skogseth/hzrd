use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

mod utils;
use crate::utils::{LinkedList, Node};

/// Place object on the heap (will leak)
fn allocate<T>(object: T) -> NonNull<T> {
    let raw = Box::into_raw(Box::new(object));
    // SAFETY: The boxed ptr itself is never null
    unsafe { NonNull::new_unchecked(raw) }
}

/// Free heap allocated memory
/// SAFETY: Must point to valid heap-allocated memory
unsafe fn free<T>(non_null_ptr: NonNull<T>) {
    let _ = Box::from_raw(non_null_ptr.as_ptr());
}

/// Holds some address that is currently used (may be null)
type HazardPtr<T> = AtomicPtr<T>;

/// `HazardCell` holds a value that can be shared, and mutated, by multiple threads
///
/// The `HazardCell` gives wait-free get and set methods to the underlying value.
/// The downside is more excessive memory use, and locking is required on `clone` & `drop`.
pub struct HazardCell<T> {
    inner: NonNull<HazardCellInner<T>>,
    node_ptr: NonNull<Node<HazardPtr<T>>>,
    marker: PhantomData<T>,
}

impl<T> HazardCell<T> {
    pub fn new(value: T) -> Self {
        let (core, node_ptr) = HazardCellInner::new(value);
        HazardCell {
            inner: allocate(core),
            node_ptr,
            marker: PhantomData,
        }
    }

    pub fn get(&self) -> HazardCellHandle<'_, T> {
        // SAFETY: These are only ever grabbed as shared
        let core = unsafe { self.inner.as_ref() };

        // SAFETY: Good?
        let ptr_to_hazard_ptr = unsafe { Node::get_from_raw(self.node_ptr.as_ptr()) };

        // SAFETY: Good right?
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

        HazardCellHandle {
            // SAFETY: Pointer is now held valid by the hazard ptr
            reference: unsafe { &*ptr },

            // SAFETY: Always non-null
            hazard_ptr: unsafe { NonNull::new_unchecked(ptr_to_hazard_ptr) },
        }
    }

    pub fn set(&self, value: T) {
        let new_ptr = Box::into_raw(Box::new(value));

        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };

        // SAFETY: Ptr must at this point be non-null
        let old_raw_ptr = core.value.swap(new_ptr, Ordering::SeqCst);
        let old_ptr = unsafe { NonNull::new_unchecked(old_raw_ptr) };

        core.retired.lock().unwrap().push_back(RetiredPtr(old_ptr));
    }

    pub fn reclaim(&self) {
        // SAFETY: Only shared references to this are allowed
        let core = unsafe { self.inner.as_ref() };
        core.reclaim();
    }
}

unsafe impl<T: Send> Send for HazardCell<T> {}

impl<T> Clone for HazardCell<T> {
    fn clone(&self) -> Self {
        // SAFETY: We can always get a shared reference to this
        let core = unsafe { self.inner.as_ref() };
        let node_ptr = core.add();

        HazardCell {
            inner: self.inner,
            node_ptr,
            marker: PhantomData,
        }
    }
}

impl<T> Drop for HazardCell<T> {
    fn drop(&mut self) {
        // SAFETY: We scope this so that all references/pointers are dropped before inner is dropped
        let should_drop_inner = {
            // SAFETY: We can always get a shared reference to this
            let core = unsafe { self.inner.as_ref() };

            // TODO: Handle panic?
            let mut hazard_ptrs = core.hazard_ptrs.lock().unwrap();

            // SAFETY: The node ptr is guaranteed to be a valid pointer to an element in the list
            let _ = unsafe { hazard_ptrs.remove_node(self.node_ptr.as_ptr()) };

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

pub struct HazardCellHandle<'cell, T> {
    reference: &'cell T,
    hazard_ptr: NonNull<HazardPtr<T>>,
}

impl<T> Deref for HazardCellHandle<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.reference
    }
}

impl<T> Drop for HazardCellHandle<'_, T> {
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

/// Shared heap allocated object for `HazardCell`
///
/// The `hazard_ptrs` keep track of pointers that are in use, and cannot be freed
/// There is one node per `HazardCell`, which means the list also keeps track
/// of the number of active `HazardCell`s (akin to a very inefficent atomic counter).
struct HazardCellInner<T> {
    value: AtomicPtr<T>,
    hazard_ptrs: Mutex<LinkedList<HazardPtr<T>>>,
    retired: Mutex<LinkedList<RetiredPtr<T>>>,
}

impl<T> HazardCellInner<T> {
    pub fn new(value: T) -> (Self, NonNull<Node<HazardPtr<T>>>) {
        let hazard_ptr = HazardPtr::new(std::ptr::null_mut());
        let ptr = Box::into_raw(Box::new(value));

        let (list, raw_node_ptr) = LinkedList::single_and_get_raw(hazard_ptr);

        let core = Self {
            value: AtomicPtr::new(ptr),
            hazard_ptrs: Mutex::new(list),
            retired: Mutex::new(LinkedList::new()),
        };

        // SAFETY: This is probably okay right?
        let node_ptr = unsafe { NonNull::new_unchecked(raw_node_ptr) };

        (core, node_ptr)
    }

    pub fn add(&self) -> NonNull<Node<HazardPtr<T>>> {
        let mut guard = self.hazard_ptrs.lock().unwrap();
        let hazard_ptr = HazardPtr::new(std::ptr::null_mut());
        let raw_node_ptr = guard.push_back_and_get_raw(hazard_ptr);

        // SAFETY: ?
        unsafe { NonNull::new_unchecked(raw_node_ptr) }
    }

    pub fn reclaim(&self) {
        let mut retired = self.retired.lock().unwrap();
        let hazard_ptrs = self.hazard_ptrs.lock().unwrap();

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
}

impl<T> Drop for HazardCellInner<T> {
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
        let _ = HazardCell::new(0);
    }

    #[test]
    fn deep_drop_test() {
        let _ = HazardCell::new(vec![1, 2, 3]);
    }

    #[test]
    fn single() {
        let string = String::new();
        let cell = HazardCell::new(string);

        {
            let handle: HazardCellHandle<_> = cell.get();
            assert_eq!(handle.len(), 0);
            assert_eq!(*handle, "");
        }

        let new_string = String::from("Hello world!");
        cell.set(new_string);

        {
            let handle: HazardCellHandle<_> = cell.get();
            assert_eq!(handle.len(), 12);
            assert_eq!(*handle, "Hello world!");
        }

        cell.reclaim();
    }

    #[test]
    fn double() {
        let string = String::new();
        let cell = HazardCell::new(string);

        std::thread::scope(|s| {
            let cell_1 = HazardCell::clone(&cell);
            s.spawn(move || {
                let handle = cell_1.get();
                std::thread::sleep(Duration::from_secs(2));
                assert_eq!(*handle, "");
            });

            std::thread::sleep(Duration::from_secs(1));

            let cell_2 = HazardCell::clone(&cell);
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
}
