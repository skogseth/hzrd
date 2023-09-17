use std::ptr::NonNull;
use std::sync::atomic::{Ordering, AtomicUsize};

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
pub struct HzrdPtr(AtomicUsize);

pub enum HzrdPtrState {
    Active(usize),
    Inactive,
    Free,
}

impl HzrdPtr {
    pub fn new() -> Self {
        HzrdPtr(AtomicUsize::new(0)) 
    }

    pub fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> HzrdPtrState {
        let val = self.0.load(Ordering::SeqCst);
        if val == 0 {
            HzrdPtrState::Inactive
        } else if val == std::ptr::addr_of!(self) as usize {
            HzrdPtrState::Free
        } else {
            HzrdPtrState::Active(val)
        }
    }

    pub unsafe fn store<T>(&self, ptr: *mut T) {
        self.0.store(ptr as usize, Ordering::SeqCst);
    }

    pub unsafe fn clear(&self) {
        self.0.store(0, Ordering::SeqCst);
    }

    pub unsafe fn free(&self) {
        let self_ptr = std::ptr::addr_of!(self) as usize;
        self.0.store(self_ptr, Ordering::SeqCst);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hzrd_ptr() {
        let mut value = String::from("Danger!");
        let hzrd_ptr = HzrdPtr::new();
        unsafe { hzrd_ptr.store(&mut value) };
        unsafe { hzrd_ptr.clear() };
        unsafe { hzrd_ptr.store(&mut value) };

        unsafe { hzrd_ptr.free() };
        unsafe { hzrd_ptr.store(&mut value) };
    }
}
