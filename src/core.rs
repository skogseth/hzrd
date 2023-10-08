use std::alloc::Layout;
use std::collections::LinkedList;
use std::ops::Deref;
use std::ptr::{addr_of, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};
use std::sync::{Mutex, RwLock};

use crate::utils::SharedCell;

/// Holds a reference to an object protected by a hazard pointer
pub struct RefHandle<'hzrd, T> {
    value: &'hzrd T,
    hzrd_ptr: &'hzrd HzrdPtr,
}

impl<T> Deref for RefHandle<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<T> Drop for RefHandle<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We are dropping so `value` will never be accessed after this
        unsafe { self.hzrd_ptr.clear() };
    }
}

/// # Safety
/// Needs to uphold the requirements of a hazard pointer domain
pub unsafe trait Domain {
    fn hzrd_ptr(&self) -> NonNull<HzrdPtr>;
    fn retire(&self, ret_ptr: RetiredPtr);
    fn reclaim(&self);
}

unsafe impl<D: Domain> Domain for &D {
    fn hzrd_ptr(&self) -> NonNull<HzrdPtr> {
        (*self).hzrd_ptr()
    }

    fn retire(&self, ret_ptr: RetiredPtr) {
        (*self).retire(ret_ptr);
    }

    fn reclaim(&self) {
        (*self).reclaim();
    }
}

pub struct SharedDomain {
    pub hzrd: RwLock<HzrdPtrs>,
    pub retired: Mutex<RetiredPtrs>,
}

impl SharedDomain {
    pub const fn new() -> Self {
        Self {
            hzrd: RwLock::new(HzrdPtrs::new()),
            retired: Mutex::new(RetiredPtrs::new()),
        }
    }
}

unsafe impl Domain for SharedDomain {
    fn hzrd_ptr(&self) -> NonNull<HzrdPtr> {
        self.hzrd.write().unwrap().get()
    }

    fn retire(&self, ret_ptr: RetiredPtr) {
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

        // Try to access the hazard pointers
        let Ok(hzrd_ptrs) = self.hzrd.try_read() else {
            return;
        };

        retired_ptrs.reclaim(&hzrd_ptrs);
    }
}

static GLOBAL_DOMAIN: SharedDomain = SharedDomain::new();

pub struct HzrdCore<T, D: Domain> {
    value: AtomicPtr<T>,
    domain: D,
}

#[allow(unused)]
impl<T> HzrdCore<T, &'static SharedDomain> {
    pub fn new(boxed: Box<T>) -> Self {
        Self {
            value: AtomicPtr::new(Box::into_raw(boxed)),
            domain: &GLOBAL_DOMAIN,
        }
    }
}

impl<T, D: Domain> HzrdCore<T, D> {
    pub fn new_in(boxed: Box<T>, domain: D) -> Self {
        let value = AtomicPtr::new(Box::into_raw(boxed));
        Self { value, domain }
    }

    fn swap(&self, boxed: Box<T>) -> RetiredPtr {
        let new_ptr = Box::into_raw(boxed);

        // SAFETY: Ptr must at this point be non-null
        let old_raw_ptr = self.value.swap(new_ptr, SeqCst);
        let non_null_ptr = unsafe { NonNull::new_unchecked(old_raw_ptr) };

        // SAFETY: We can guarantee it's pointing to heap-allocated memory
        unsafe { RetiredPtr::new(non_null_ptr) }
    }

    pub fn set(&self, boxed: Box<T>) {
        self.just_set(boxed);
        self.domain.reclaim();
    }

    pub fn just_set(&self, boxed: Box<T>) {
        let old_ptr = self.swap(boxed);
        self.domain.retire(old_ptr);
    }

    pub fn reclaim(&self) {
        self.domain.reclaim();
    }

    pub fn hzrd_ptr(&self) -> NonNull<HzrdPtr> {
        self.domain.hzrd_ptr()
    }

    pub unsafe fn read<'hzrd>(&self, hzrd_ptr: &'hzrd HzrdPtr) -> RefHandle<'hzrd, T> {
        let mut ptr = self.value.load(SeqCst);
        // SAFETY: Non-null ptr
        unsafe { hzrd_ptr.store(ptr) };

        // We now need to keep updating it until it is in a consistent state
        loop {
            ptr = self.value.load(SeqCst);
            if ptr as usize == hzrd_ptr.get() {
                break;
            } else {
                // SAFETY: Non-null ptr
                unsafe { hzrd_ptr.store(ptr) };
            }
        }

        // SAFETY: This pointer is now held valid by the hazard pointer
        let value = unsafe { &*ptr };
        RefHandle { value, hzrd_ptr }
    }

    pub fn domain(&self) -> &D {
        &self.domain
    }
}

impl<T, D: Domain> Drop for HzrdCore<T, D> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(SeqCst)) };
    }
}

// SAFETY: Is this correct?
unsafe impl<T: Sync, D: Domain + Sync> Sync for HzrdCore<T, D> {}

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
        self.0.load(Relaxed) == 0
    }

    pub fn try_take(&self) -> Option<&Self> {
        match self.0.compare_exchange(0, dummy_addr(), AcqRel, Relaxed) {
            Ok(_) => Some(self),
            Err(_) => None,
        }
    }

    pub unsafe fn store<T>(&self, ptr: *mut T) {
        self.0.store(ptr as usize, SeqCst);
    }

    pub unsafe fn clear(&self) {
        self.0.store(dummy_addr(), Release);
    }

    pub unsafe fn free(&self) {
        self.0.store(0, Release);
    }
}

pub struct HzrdPtrs(LinkedList<SharedCell<HzrdPtr>>);

impl HzrdPtrs {
    pub const fn new() -> Self {
        Self(LinkedList::new())
    }

    /// Get a new HzrdPtr (this may allocate a new node in the list)
    pub fn get(&mut self) -> NonNull<HzrdPtr> {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        for node in self.0.iter() {
            if let Some(hzrd_ptr) = node.get().try_take() {
                return NonNull::from(hzrd_ptr);
            }
        }

        self.0.push_back(SharedCell::new(HzrdPtr::new()));

        // SAFETY: We pushed to the list, it must be non-empty
        let hzrd_ptr = unsafe { self.0.back().unwrap_unchecked().get() };

        NonNull::from(hzrd_ptr)
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.0.iter().any(|node| node.get().get() == addr)
    }

    pub fn all_available(&self) -> bool {
        self.0.iter().all(|node| node.get().is_available())
    }
}

impl Default for HzrdPtrs {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RetiredPtr {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl RetiredPtr {
    // SAFETY: Must point to heap-allocated value
    pub unsafe fn new<T>(ptr: NonNull<T>) -> Self {
        RetiredPtr {
            ptr: ptr.cast(),
            layout: Layout::new::<T>(),
        }
    }

    pub fn addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
}

impl Drop for RetiredPtr {
    fn drop(&mut self) {
        // SAFETY: No reference to this when dropped (and always heap allocated)
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

unsafe impl Send for RetiredPtr {}
unsafe impl Sync for RetiredPtr {}

pub struct RetiredPtrs(Vec<RetiredPtr>);

impl RetiredPtrs {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn add(&mut self, val: RetiredPtr) {
        self.0.push(val);
    }

    pub fn reclaim(&mut self, hzrd_ptrs: &HzrdPtrs) {
        self.0.retain(|p| hzrd_ptrs.contains(p.addr()));
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

    #[test]
    fn retirement() {
        let value = Box::into_raw(Box::new([1, 2, 3]));
        let value = unsafe { NonNull::new_unchecked(value) };

        let mut hzrd_ptrs = HzrdPtrs::new();
        let mut retired = RetiredPtrs::new();

        let hzrd_ptr = unsafe { hzrd_ptrs.get().as_ref() };
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
}
