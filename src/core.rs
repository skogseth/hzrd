use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

use crate::linked_list::LinkedList;
use crate::ptr::{HzrdPtr, HzrdPtrs};
use crate::utils::RetiredPtr;
use crate::RefHandle;

/// Shared heap allocated object for `HzrdCell`
///
/// The `hzrd_ptrs` keep track of pointers that are in use, and cannot be freed
/// There is one node per `HzrdCell`, which means the list also keeps track
/// of the number of active `HzrdCell`s (akin to a very inefficent atomic counter).
pub struct HzrdCellInner<T> {
    pub value: AtomicPtr<T>,
    pub hzrd_ptrs: Mutex<HzrdPtrs>,
    pub retired: Mutex<LinkedList<RetiredPtr<T>>>,
}

impl<T> HzrdCellInner<T> {
    pub fn new(boxed: Box<T>) -> (Self, NonNull<HzrdPtr>) {
        let ptr = Box::into_raw(boxed);

        let mut hzrd_ptrs = HzrdPtrs::new();
        let hzrd_ptr = hzrd_ptrs.get();

        let core = Self {
            value: AtomicPtr::new(ptr),
            hzrd_ptrs: Mutex::new(hzrd_ptrs),
            retired: Mutex::new(LinkedList::new()),
        };

        (core, hzrd_ptr)
    }

    /// Reads the contained value and keeps it valid through the hazard pointer
    /// SAFETY:
    /// - Can only be called by the owner of the hazard pointer
    /// - The owner cannot call this again until the [`ReadHandle`] has been dropped
    pub unsafe fn read<'hzrd>(&self, hzrd_ptr: &'hzrd HzrdPtr) -> RefHandle<'hzrd, T> {
        let mut ptr = self.value.load(Ordering::SeqCst);
        hzrd_ptr.store(ptr);

        // We now need to keep updating it until it is in a consistent state
        loop {
            ptr = self.value.load(Ordering::SeqCst);
            if ptr as usize == hzrd_ptr.get() {
                break;
            } else {
                hzrd_ptr.store(ptr);
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = &*ptr;
        RefHandle { value, hzrd_ptr }
    }

    pub fn swap(&self, value: T) -> NonNull<T> {
        let new_ptr = Box::into_raw(Box::new(value));

        // SAFETY: Ptr must at this point be non-null
        let old_raw_ptr = self.value.swap(new_ptr, Ordering::SeqCst);
        unsafe { NonNull::new_unchecked(old_raw_ptr) }
    }

    pub fn add(&self) -> NonNull<HzrdPtr> {
        self.hzrd_ptrs.lock().unwrap().get()
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
        let hzrd_ptrs = self.hzrd_ptrs.lock().unwrap();

        reclaim(&mut retired, &hzrd_ptrs);
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
        let Ok(hzrd_ptrs) = self.hzrd_ptrs.try_lock() else {
            return;
        };

        reclaim(&mut retired, &hzrd_ptrs);
    }
}

impl<T> Drop for HzrdCellInner<T> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(Ordering::SeqCst)) };
    }
}

pub fn reclaim<T>(retired_ptrs: &mut LinkedList<RetiredPtr<T>>, hzrd_ptrs: &HzrdPtrs) {
    let mut still_active = LinkedList::new();
    'outer: while let Some(retired_ptr) = retired_ptrs.pop_front() {
        if hzrd_ptrs.contains(retired_ptr.as_ptr() as usize) {
            still_active.push_back(retired_ptr);
            continue 'outer;
        }
    }

    *retired_ptrs = still_active;
}
