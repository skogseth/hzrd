/*!
Module containing core functionality for this crate.

The most imporant part of this module is the [`Domain`] trait, as it defines the interface for any type of domain. There are multiple domains implemented in this crate (implementing one yourself is no easy task), all of which can be found in the [`domains`](`crate::domains`)-module. The default domain used by [`HzrdCell`](`crate::HzrdCell`) is [`GlobalDomain`](`crate::domains::GlobalDomain`).

There are also two very important types in this module:
- [`HzrdPtr`]
- [`RetiredPtr`]

These are used in the [`Domain`] interface, and can be considered the fundamental building blocks of the library.
*/

// -------------------------------------

use std::ops::Deref;
use std::ptr::{addr_of, NonNull};
use std::rc::Rc;
use std::sync::atomic::Ordering::*;
use std::sync::atomic::{AtomicPtr, AtomicUsize};
use std::sync::Arc;

// ------------------------------

/// Action performed on hazard pointer on drop of [`ReadHandle`]
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// Reset hazard pointer
    Reset,
    /// Release hazard pointer
    Release,
}

/**
Holds a reference to a read value. The value is kept alive by a hazard pointer.

Note that the reference held by the handle is to the value as it was when it was read.
If the cell is written to during the lifetime of the handle this will not be reflected in its value.

# Example
```
# use hzrd::HzrdCell;
let cell = HzrdCell::new(vec![1, 2, 3, 4]);

// Read the value and hold on to a reference to the value
let handle = cell.read();
assert_eq!(handle[..], [1, 2, 3, 4]);

// NOTE: The value is not updated when the cell is written to
cell.set(Vec::new());
assert_eq!(handle[..], [1, 2, 3, 4]);
```
*/
#[derive(Debug)]
pub struct ReadHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr,
    action: Action,
}

impl<'hzrd, T> ReadHandle<'hzrd, T> {
    /**
    Read value of an atomic pointer and protect the reference using a hazard pointer.

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    - The value of the atomic pointer must be protected by the given hazard pointer
    - The hazard pointer must be correctly handled with respect to the action performed on drop

    # Example
    ```
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicPtr, Ordering::*};

    use hzrd::core::{Action, Domain, ReadHandle, RetiredPtr};
    use hzrd::domains::GlobalDomain;

    let value = AtomicPtr::new(Box::into_raw(Box::new(false)));
    let domain = GlobalDomain;

    let set_value = |new_value| {
        let new_ptr = Box::into_raw(Box::new(new_value));
        let old_ptr = value.swap(new_ptr, SeqCst);
        let non_null_ptr = unsafe { NonNull::new_unchecked(old_ptr) };
        domain.retire(unsafe { RetiredPtr::new(non_null_ptr) });
    };

    std::thread::scope(|s| {
        s.spawn(|| {
            let hzrd_ptr = domain.hzrd_ptr();
            let state = unsafe { ReadHandle::read_unchecked(&value, hzrd_ptr, Action::Release) };
            println!("{}", *state);
        });

        set_value(true);
    });

    // Clean up the value still held by the atomic pointer
    let _ = unsafe { Box::from_raw(value.load(SeqCst)) };
    ```
    */
    pub unsafe fn read_unchecked(
        value: &'hzrd AtomicPtr<T>,
        hzrd_ptr: &'hzrd HzrdPtr,
        action: Action,
    ) -> Self {
        let mut ptr = value.load(SeqCst);
        loop {
            // SAFETY: ptr is not null
            unsafe { hzrd_ptr.protect(ptr) };

            // We now need to keep updating it until it is in a consistent state
            let new_ptr = value.load(SeqCst);
            if ptr == new_ptr {
                break;
            } else {
                ptr = new_ptr;
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = unsafe { &*ptr };

        Self {
            value,
            hzrd_ptr,
            action,
        }
    }
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
        match self.action {
            Action::Reset => unsafe { self.hzrd_ptr.reset() },
            Action::Release => unsafe { self.hzrd_ptr.release() },
        }
    }
}

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
    ///
    /// The method must return the number of reclaimed objects
    fn reclaim(&self) -> usize;

    // -------------------------------------

    /// Retire the provided retired-pointer and reclaim all "reclaimable" memory
    ///
    /// The method must return the number of reclaimed objects
    fn retire(&self, ret_ptr: RetiredPtr) -> usize {
        self.just_retire(ret_ptr);
        self.reclaim()
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

            fn reclaim(&self) -> usize {
                (**self).reclaim()
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
        self.0.load(SeqCst)
    }

    /// Try to aquire the hazard pointer
    pub fn try_acquire(&self) -> Option<&Self> {
        match self.0.compare_exchange(0, dummy_addr(), SeqCst, Relaxed) {
            Ok(_) => Some(self),
            Err(_) => None,
        }
    }

    /**
    Protect the value behind this pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    - The caller must assert that the ptr did not change before the value was stored
    - The pointer may not be null
    */
    pub unsafe fn protect<T>(&self, ptr: *mut T) {
        debug_assert!(!ptr.is_null());
        self.0.store(ptr as usize, SeqCst);
    }

    /**
    Reset the hazard pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    */
    pub unsafe fn reset(&self) {
        self.0.store(dummy_addr(), SeqCst);
    }

    /**
    Release the hazard pointer

    # Safety
    - The caller must be the current "owner" of the hazard pointer
    - The hazard cell must be re-aquired after calling this using [`try_acquire`](`HzrdPtr::try_acquire`)
    */
    pub unsafe fn release(&self) {
        self.0.store(0, SeqCst);
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

trait Delete {}
impl<T> Delete for T {}

/// A retired pointer that will free the underlying value on drop
pub struct RetiredPtr {
    ptr: NonNull<dyn Delete>,
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
