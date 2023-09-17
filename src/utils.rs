use std::ptr::NonNull;

use crate::linked_list::LinkedList;
use crate::ptr::HzrdPtrs;

/// Place object on the heap (will leak)
pub fn allocate<T>(object: T) -> NonNull<T> {
    let raw = Box::into_raw(Box::new(object));
    // SAFETY: The boxed ptr itself is never null
    unsafe { NonNull::new_unchecked(raw) }
}

/// Free heap allocated memory
/// SAFETY: Must point to valid heap-allocated memory
pub unsafe fn free<T>(non_null_ptr: NonNull<T>) {
    let _ = Box::from_raw(non_null_ptr.as_ptr());
}

pub struct RetiredPtr<T>(NonNull<T>);

impl<T> RetiredPtr<T> {
    pub fn new(ptr: NonNull<T>) -> Self {
        RetiredPtr(ptr)
    }

    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }
}

impl<T> Drop for RetiredPtr<T> {
    fn drop(&mut self) {
        // SAFETY: No reference to this when dropped
        unsafe { free(self.0) };
    }
}

pub fn reclaim<T>(retired_ptrs: &mut LinkedList<RetiredPtr<T>>, hzrd_ptrs: &HzrdPtrs) {
    let mut still_active = LinkedList::new();
    while let Some(retired_ptr) = retired_ptrs.pop_front() {
        if hzrd_ptrs.contains(retired_ptr.as_ptr() as usize) {
            still_active.push_back(retired_ptr);
            continue;
        }
    }

    *retired_ptrs = still_active;
}
