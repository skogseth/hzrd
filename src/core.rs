use std::ops::Deref;
use std::ptr::{addr_of, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};

use crate::linked_list::LinkedList;

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

/// Function related to reading hazard pointer protected values
///
/// # Safety
/// Type cannot implement `Sync`
pub unsafe trait Read {
    type T;

    // SAFETY: Only one `RefHandle` can exist at any given point
    unsafe fn read_unchecked(&self) -> RefHandle<Self::T>;

    fn read(&mut self) -> RefHandle<Self::T> {
        // SAFETY: We hold a mutable reference
        unsafe { self.read_unchecked() }
    }

    fn get(&self) -> Self::T
    where
        Self::T: Copy,
    {
        // SAFETY: Copy value and drop `RefHandle` immediately
        unsafe { *self.read_unchecked() }
    }

    fn cloned(&self) -> Self::T
    where
        Self::T: Clone,
    {
        // SAFETY: Clone value and drop `RefHandle` immediately
        unsafe { self.read_unchecked().clone() }
    }

    fn read_and_map<U, F: FnOnce(&Self::T) -> U>(&self, f: F) -> U {
        // SAFETY:
        // - Drop handle at the end of the function
        // - We don't access the hazard pointer for the rest of the function
        let value = unsafe { self.read_unchecked() };

        f(&value)
    }
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

pub struct HzrdPtrs(LinkedList<HzrdPtr>);

impl HzrdPtrs {
    pub fn new() -> Self {
        Self(LinkedList::new())
    }

    /// Get a new HzrdPtr (this may allocate a new node in the list)
    pub fn get(&mut self) -> NonNull<HzrdPtr> {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        for node in self.0.iter() {
            if let Some(hzrd_ptr) = node.try_take() {
                return NonNull::from(hzrd_ptr);
            }
        }

        let hzrd_ptr = self.0.push_back(HzrdPtr::new());
        NonNull::from(hzrd_ptr)
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.0.iter().any(|node| node.get() == addr)
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
    pub unsafe fn read<'hzrd>(core: &HzrdCore<T>, hzrd_ptr: &'hzrd HzrdPtr) -> RefHandle<'hzrd, T> {
        let mut ptr = core.value.load(SeqCst);
        hzrd_ptr.store(ptr);

        // We now need to keep updating it until it is in a consistent state
        loop {
            ptr = core.value.load(SeqCst);
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
        let old_raw_ptr = self.value.swap(new_ptr, SeqCst);
        unsafe { NonNull::new_unchecked(old_raw_ptr) }
    }
}

impl<T> Drop for HzrdCore<T> {
    fn drop(&mut self) {
        // SAFETY: No more references can be held if this is being dropped
        let _ = unsafe { Box::from_raw(self.value.load(SeqCst)) };
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
