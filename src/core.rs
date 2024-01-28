use std::any::Any;
use std::ptr::{addr_of, NonNull};
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::*;
use std::sync::Arc;

// -------------------------------------

/**
A trait describing a hazard pointer domain

A hazard pointer domain contains a set of given hazard pointers. A value protected by hazard pointers belong to a given domain. When the value is swapped the "swapped-out-value" should be retired to the domain associated with the value, such that it is properly cleaned up when there are no more hazard pointers guarding the reclamation of the value.

# Safety
Implementing `Domain` is marked `unsafe` as a correct implementation is relied upon by the types of this crate. A sound implementation of `Domain` requires the type to only free [`RetiredPtr`]s passed in via [`retire`](`Domain::retire`)/[`just_retire`](`Domain::just_retire`) if no [`HzrdPtr`]s given out by this function is not protecting the value. A good implementation should free these pointers when both [`reclaim`](`Domain::reclaim`) is called, as well as after updating the value in [`retire`](`Domain::retire`).
*/
pub unsafe trait Domain {
    /**
    Get a new hazard pointer in the given domain

    This function may allocate a new hazard pointer in the domain.
    This should, ideally, only happen if there are none available.
    */
    fn hzrd_ptr(&self) -> &HzrdPtr;

    /// Retire the provided retired-pointer, but don't reclaim memory
    fn just_retire(&self, ret_ptr: RetiredPtr);

    /// Reclaim all "reclaimable" memory in the given domain
    fn reclaim(&self);

    // -------------------------------------

    /// Retire the provided retired-pointer and reclaim all "reclaimable" memory
    fn retire(&self, ret_ptr: RetiredPtr) {
        self.just_retire(ret_ptr);
        self.reclaim();
    }
}

// https://stackoverflow.com/questions/63963544/automatically-derive-traits-implementation-for-arc
macro_rules! deref_impl {
    ($($sig:tt)+) => {
        unsafe impl $($sig)+ {
            fn hzrd_ptr(&self) -> &HzrdPtr {
                (**self).hzrd_ptr()
            }

            fn just_retire(&self, ret_ptr: RetiredPtr) {
                (**self).just_retire(ret_ptr);
            }

            fn reclaim(&self) {
                (**self).reclaim();
            }
        }
    };
}

deref_impl!(<D: Domain> Domain for &D);
deref_impl!(<D: Domain> Domain for Rc<D>);
deref_impl!(<D: Domain> Domain for Arc<D>);

// -------------------------------------

fn dummy_addr() -> usize {
    static DUMMY: u8 = 0;
    addr_of!(DUMMY) as usize
}

/// Holds some address that is currently used
pub struct HzrdPtr(AtomicUsize);

impl HzrdPtr {
    /// Create a new hazard pointer (it will already be acquired)
    pub fn new() -> Self {
        HzrdPtr(AtomicUsize::new(dummy_addr()))
    }

    /// Get the value held by the hazard pointer
    pub fn get(&self) -> usize {
        self.0.load(Acquire)
    }

    /// Try to aquire the hazard pointer
    pub fn try_acquire(&self) -> Option<&Self> {
        match self.0.compare_exchange(0, dummy_addr(), Relaxed, Relaxed) {
            Ok(_) => Some(self),
            Err(_) => None,
        }
    }

    /**
    Protect the value behind this pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    - The caller must assert that the ptr did not change before the value was stored
    */
    pub unsafe fn protect<T>(&self, ptr: *mut T) {
        self.0.store(ptr as usize, Release);
    }

    /**
    Reset the hazard pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    */
    pub unsafe fn reset(&self) {
        self.0.store(dummy_addr(), Release);
    }

    /**
    Release the hazard pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    - The hazard cell must be reaquired after calling this using [`try_acquire`](`HzrdPtr::try_acquire`)
    */
    pub unsafe fn release(&self) {
        self.0.store(0, Release);
    }
}

impl Default for HzrdPtr {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HzrdPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HzrdPtr({:#x})", self.0.load(Relaxed))
    }
}

unsafe impl Send for HzrdPtr {}
unsafe impl Sync for HzrdPtr {}

// -------------------------------------

/// A retired pointer that will free the underlying value on drop
pub struct RetiredPtr {
    ptr: NonNull<dyn Any>,
}

impl RetiredPtr {
    /**
    Create a new retired pointer

    # Safety
    - The input pointer must point to heap-allocated value.
    - The pointer must be held alive until it is safe to drop
    */
    pub unsafe fn new<T: 'static>(ptr: NonNull<T>) -> Self {
        RetiredPtr { ptr }
    }

    /// Get the address of the retired pointer
    pub fn addr(&self) -> usize {
        self.ptr.as_ptr() as *mut () as usize
    }
}

impl Drop for RetiredPtr {
    fn drop(&mut self) {
        // SAFETY: No reference to this when dropped (and always heap allocated)
        let _ = unsafe { Box::from_raw(self.ptr.as_ptr()) };
    }
}

impl std::fmt::Debug for RetiredPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RetiredPtr({:#x})", self.addr())
    }
}

unsafe impl Send for RetiredPtr {}
unsafe impl Sync for RetiredPtr {}

// -------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hzrd_ptr() {
        let mut value = String::from("Danger!");
        let hzrd_ptr = HzrdPtr::new();
        unsafe { hzrd_ptr.protect(&mut value) };
        unsafe { hzrd_ptr.reset() };
        unsafe { hzrd_ptr.protect(&mut value) };

        unsafe { hzrd_ptr.release() };
        unsafe { hzrd_ptr.protect(&mut value) };
    }

    #[test]
    fn retired_ptr() {
        let object = vec![String::from("Hello"), String::from("World")];
        let ptr = NonNull::from(Box::leak(Box::new(object)));

        // SAFETY: ptr is heap-allocated
        let retired = unsafe { RetiredPtr::new(ptr) };
        drop(retired);
    }
}
