use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

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

/// Holds some address that is currently used (may be null)
pub struct HzrdPtr<T>(AtomicPtr<T>);

impl<T> HzrdPtr<T> {
    pub fn new() -> Self {
        HzrdPtr(AtomicPtr::new(null_mut()))
    }

    pub fn get(&self) -> *mut T {
        self.0.load(Ordering::SeqCst)
    }

    pub unsafe fn store(&self, ptr: *mut T) {
        self.0.store(ptr, Ordering::SeqCst);
    }

    pub unsafe fn clear(&self) {
        self.0.store(null_mut(), Ordering::SeqCst);
    }
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
