use std::ptr::{addr_of, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering::*};

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

fn dummy_addr() -> usize {
    static DUMMY: u8 = 0;
    addr_of!(DUMMY) as usize
}

/// Holds some address that is currently used (may be null)
pub struct HzrdPtr(AtomicUsize);

impl HzrdPtr {
    pub fn new() -> Self {
        HzrdPtr(AtomicUsize::new(dummy_addr()))
    }

    pub fn get(&self) -> usize {
        self.0.load(SeqCst)
    }

    pub fn is_available(&self) -> bool {
        self.0.load(SeqCst) == 0
    }

    pub fn try_take(&self) -> Option<&Self> {
        let took = self.0.compare_exchange(0, dummy_addr(), SeqCst, SeqCst);
        todo!()
    }

    pub unsafe fn store<T>(&self, ptr: *mut T) {
        self.0.store(ptr as usize, SeqCst);
    }

    pub unsafe fn clear(&self) {
        self.0.store(dummy_addr(), SeqCst);
    }

    pub unsafe fn free(&self) {
        self.0.store(0, SeqCst);
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
