use std::any::Any;
use std::collections::BTreeSet;
use std::ptr::{addr_of, NonNull};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use crate::stack::SharedStack;

/**
A trait describing a hazard pointer domain

A hazard pointer domain contains a set of given hazard pointers. A value protected by hazard pointers belong to a given domain. When the value is swapped the "swapped-out-value" should be retired to the domain associated with the value, such that it is properly cleaned up when there are no more hazard pointers guarding the reclamation of the value.

# Safety
The list of hazard pointers and retired pointers should, in general, not be mutated outside of these functions (e.g. by removing elements from the list).
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

#[derive(Debug)]
pub struct SharedDomain {
    pub(crate) hzrd: HzrdPtrs,
    pub(crate) retired: Mutex<RetiredPtrs>,
}

impl SharedDomain {
    pub const fn new() -> Self {
        Self {
            hzrd: HzrdPtrs::new(),
            retired: Mutex::new(RetiredPtrs::new()),
        }
    }

    #[cfg(test)]
    pub fn number_of_retired_ptrs(&self) -> usize {
        self.retired.lock().unwrap().len()
    }
}

unsafe impl Domain for SharedDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        self.hzrd.get()
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        self.retired.lock().unwrap().add(ret_ptr);
    }

    fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken
        let Ok(mut retired_ptrs) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        retired_ptrs.reclaim(&self.hzrd);
    }

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        // Grab the lock to retired pointers
        let mut retired_ptrs = self.retired.lock().unwrap();

        // And retire the given pointer
        retired_ptrs.add(ret_ptr);

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        retired_ptrs.reclaim(&self.hzrd);
    }
}

// TODO: Introduce LocalDomain (`Send` but not `Sync`, use UnsafeCell + LinkedList & Vec)

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
        match self.0.compare_exchange(0, dummy_addr(), SeqCst, SeqCst) {
            Ok(_) => Some(self),
            Err(_) => None,
        }
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

impl std::fmt::Debug for HzrdPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HzrdPtr({:#x})", self.0.load(SeqCst))
    }
}

#[derive(Debug)]
pub struct HzrdPtrs(SharedStack<HzrdPtr>);

impl HzrdPtrs {
    pub const fn new() -> Self {
        Self(SharedStack::new())
    }

    /// Get a new HzrdPtr (this may allocate a new node in the list)
    pub fn get(&self) -> &HzrdPtr {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        for node in self.0.iter() {
            if let Some(hzrd_ptr) = node.try_take() {
                return hzrd_ptr;
            }
        }

        self.0.push(HzrdPtr::new())
    }

    pub fn count(&self) -> usize {
        self.0.iter().count()
    }

    pub fn iter(&self) -> impl Iterator<Item = &HzrdPtr> {
        self.0.iter()
    }

    pub fn all_available(&self) -> bool {
        self.0.iter().all(|node| node.is_available())
    }
}

impl Default for HzrdPtrs {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct RetiredPtr {
    ptr: NonNull<dyn Any>,
}

impl RetiredPtr {
    // SAFETY: Must point to heap-allocated value
    pub unsafe fn new<T: 'static>(ptr: NonNull<T>) -> Self {
        RetiredPtr { ptr }
    }

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

unsafe impl Send for RetiredPtr {}
unsafe impl Sync for RetiredPtr {}

#[derive(Debug)]
pub struct RetiredPtrs(Vec<RetiredPtr>);

impl RetiredPtrs {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn add(&mut self, val: RetiredPtr) {
        self.0.push(val);
    }

    pub fn reclaim(&mut self, hzrd_ptrs: &HzrdPtrs) {
        let hzrd_ptrs: BTreeSet<_> = hzrd_ptrs.iter().map(HzrdPtr::get).collect();
        // dbg!(&hzrd_ptrs);
        // dbg!(&self);
        self.0.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl Default for RetiredPtrs {
    fn default() -> Self {
        Self::new()
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

    fn new_value<T>(value: T) -> NonNull<T> {
        let boxed = Box::new(value);
        let raw = Box::into_raw(boxed);
        unsafe { NonNull::new_unchecked(raw) }
    }

    #[test]
    fn retirement() {
        let value = new_value([1, 2, 3]);

        let hzrd_ptrs = HzrdPtrs::new();
        let mut retired = RetiredPtrs::new();

        let hzrd_ptr = hzrd_ptrs.get();
        unsafe { hzrd_ptr.store(value.as_ptr()) };

        retired.add(unsafe { RetiredPtr::new(value) });
        assert_eq!(retired.len(), 1);

        retired.reclaim(&hzrd_ptrs);
        assert_eq!(retired.len(), 1);

        unsafe { hzrd_ptr.clear() };
        retired.reclaim(&hzrd_ptrs);
        assert_eq!(retired.len(), 0);
        assert!(retired.is_empty());
    }

    #[test]
    fn domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = SharedDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        unsafe { hzrd_ptr.store(ptr.as_ptr()) };
        let set: BTreeSet<_> = domain.hzrd.iter().map(HzrdPtr::get).collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
    }

    #[test]
    fn deep_leak() {
        let object = vec![String::from("Hello"), String::from("World")];
        let ptr = NonNull::from(Box::leak(Box::new(object)));

        // SAFETY: ptr is heap-allocated
        let retired = unsafe { RetiredPtr::new(ptr) };
        drop(retired);
    }
}
