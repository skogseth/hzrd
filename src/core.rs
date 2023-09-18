use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::ptr::HzrdPtr;
use crate::RefHandle;

pub struct HzrdCore<T> {
    value: AtomicPtr<T>,
}

impl<T> HzrdCore<T> {
    pub fn new(boxed: Box<T>) -> Self {
        let value = AtomicPtr::new(Box::into_raw(boxed));
        Self { value }
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
}

impl<T> Drop for HzrdCore<T> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(Ordering::SeqCst)) };
    }
}
