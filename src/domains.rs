/*!
Module containing various types implementing the [`Domain`](`crate::core::Domain`)-trait.

The module has three core types:
- [`GlobalDomain`]: A multithreaded, globally shared domain (default)
- [`SharedDomain`]: A multithreaded, shared domain
- [`LocalDomain`]: A singlethreaded, local domain

The default domain used by [`HzrdCell`] is [`GlobalDomain`], which is the recommended domain for most applications.
*/
use std::cell::UnsafeCell;
use std::collections::{BTreeSet, LinkedList};
use std::sync::Mutex;

use crate::core::{Domain, HzrdPtr, RetiredPtr};
use crate::stack::SharedStack;

// -------------------------------------

/*
# Todo:
- Add options for caching:
  -> No caching
  -> Maximum size of cache?
  -> Fixed size for cache?
  -> Pre-allocated cache?
- Add option for bulk-reclaim (default to what?), use const generics?
- Test HashSet for cache (BTreeSet can't reuse allocation?)

*/

// -------------------------------------

thread_local! {
    static HAZARD_POINTERS_CACHE: UnsafeCell<Vec<usize>> = const { UnsafeCell::new(Vec::new()) };
}

pub struct HzrdPtrsCache(*mut Vec<usize>);

impl HzrdPtrsCache {
    /// SAFETY: Only one object of this type can exist at any point
    unsafe fn load<'t>(hzrd_ptrs: impl Iterator<Item = &'t HzrdPtr>) -> Self {
        let hzrd_ptrs_cache: *mut Vec<usize> = HAZARD_POINTERS_CACHE.with(|cell| cell.get());
        unsafe { &mut *hzrd_ptrs_cache }.clear();
        unsafe { &mut *hzrd_ptrs_cache }.extend(hzrd_ptrs.map(HzrdPtr::get));
        Self(hzrd_ptrs_cache)
    }

    fn contains(&self, addr: usize) -> bool {
        unsafe { &*(self.0) }.contains(&addr)
    }
}

// -------------------------------------

static HAZARD_POINTERS: SharedStack<HzrdPtr> = SharedStack::new();

static SHARED_RETIRED_POINTERS: Mutex<Vec<RetiredPtr>> = Mutex::new(Vec::new());

thread_local! {
    static LOCAL_RETIRED_POINTERS: LocalRetiredPtrs = const { LocalRetiredPtrs(UnsafeCell::new(Vec::new())) };
}

/**
We need a special wrapper type to handle cleanup on closing threads.

There is a potential for memory leaks if the drop function is not called, which can happen according to https://doc.rust-lang.org/std/thread/struct.LocalKey.html. It seems like we're in the clear, though.
*/
struct LocalRetiredPtrs(UnsafeCell<Vec<RetiredPtr>>);

impl Drop for LocalRetiredPtrs {
    fn drop(&mut self) {
        // We can actually use `get_mut` in here, nice!
        let local_retired_ptrs = self.0.get_mut();

        // Clean up any garbage that can be cleaned up
        let hzrd_ptrs: BTreeSet<_> = HAZARD_POINTERS.iter().map(HzrdPtr::get).collect();
        local_retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));

        // If there's still garbage we send it to the shared pool
        if !local_retired_ptrs.is_empty() {
            let mut shared_retired_ptrs = SHARED_RETIRED_POINTERS.lock().unwrap();
            shared_retired_ptrs.extend(local_retired_ptrs.drain(..));
        }
    }
}

impl std::fmt::Debug for LocalRetiredPtrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = f.debug_tuple("LocalRetiredPtrs");
        f.field(unsafe { &*self.0.get() });
        f.finish()
    }
}

/**
A globally shared, multithreaded domain

Here is a code example explaing a little bit of how it works:

```
use hzrd::{HzrdCell, GlobalDomain};

// We here explicitly mark the use of the `GlobalDomain`
let cell_1 = HzrdCell::new_in(0, GlobalDomain);

// We usually just use the default constructor `HzrdCell::new`
let cell_2 = HzrdCell::new(false);

// We read the value of the two cells, holding on to the handle for now
let _handle_1 = cell_1.read();
let _handle_2 = cell_2.read();

// The `GlobalDomain` now holds two hazard pointers
// Both of which are at the moment in active use in `handle_1` and `handle_2`, respectively

// We write some values to the cells, which will not be able to free the previous
// values in the cell as there are references to these in `handle_1` and `handle_2`
cell_1.set(1);
cell_2.set(true);

// The `GlobalDomain` now has the following garbage: ( 0, false )

// Drop both handles, so garbage can (eventually) be freed
drop(handle_1);
drop(handle_2);

// Free all garbage in the `GlobalDomain`
cell_1.reclaim();

// There is no need to call this on cell_2 as they both share the `GlobalDomain`.
```

Technically there is some more complexity to the garbage collection in `GlobalDomain`. Each thread holds its own garbage, as well as access to the shared garbage. If a thread closes down with garbage still remaining (it will attempt one last cleanup before closing), then that garbage will be placed in the shared garbage. Whenever a thread does garbage collection it will first try to clean up the local garbage it holds, followed by an attempt to clean up the shared garbage. However, since the shared garbage is locked by a [`Mutex`](`std::sync::Mutex`) it will only attempt to do so. If the shared garbage is locked by another thread it will simply skip it.
*/
pub struct GlobalDomain;

impl GlobalDomain {
    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        HAZARD_POINTERS.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        LOCAL_RETIRED_POINTERS.with(|cell| {
            let retired_ptrs = unsafe { &*cell.0.get() };
            retired_ptrs.len()
        })
    }
}

unsafe impl Domain for GlobalDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        match HAZARD_POINTERS.iter().find_map(|node| node.try_acquire()) {
            Some(hzrd_ptr) => hzrd_ptr,
            None => HAZARD_POINTERS.push(HzrdPtr::new()),
        }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        LOCAL_RETIRED_POINTERS.with(|cell| {
            let retired_ptrs = unsafe { &mut *cell.0.get() };
            retired_ptrs.push(ret_ptr)
        })
    }

    fn reclaim(&self) {
        // SAFETY: We only use this in the domain functions and always drop it
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(HAZARD_POINTERS.iter()) };

        LOCAL_RETIRED_POINTERS.with(|cell| {
            let retired_ptrs = unsafe { &mut *cell.0.get() };
            retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        });

        if let Ok(mut retired_ptrs) = SHARED_RETIRED_POINTERS.try_lock() {
            retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        }
    }

    // -------------------------------------

    // Override this to avoid mutable aliasing
    fn retire(&self, ret_ptr: RetiredPtr) {
        // SAFETY: We only use this in the domain functions and always drop it
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(HAZARD_POINTERS.iter()) };

        LOCAL_RETIRED_POINTERS.with(|cell| {
            let retired_ptrs = unsafe { &mut *cell.0.get() };
            retired_ptrs.push(ret_ptr);
            retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        });

        if let Ok(mut retired_ptrs) = SHARED_RETIRED_POINTERS.try_lock() {
            retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        }
    }
}

impl std::fmt::Debug for GlobalDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = f.debug_struct("GlobalDomain");
        f.field("hzrd", &HAZARD_POINTERS);
        f.field("shared_retired", &SHARED_RETIRED_POINTERS);
        f.field("local_retired", &LOCAL_RETIRED_POINTERS);
        f.finish_non_exhaustive()
    }
}

// ------------------------------------------

/// Shared, multithreaded domain
#[derive(Debug)]
pub struct SharedDomain {
    hzrd_ptrs: SharedStack<HzrdPtr>,
    retired_ptrs: Mutex<Vec<RetiredPtr>>,
}

impl SharedDomain {
    /**
    Construct a new, clean shared domain

    # Example
    ```
    # use hzrd::SharedDomain;
    let domain = SharedDomain::new();
    ```
    */
    pub const fn new() -> Self {
        Self {
            hzrd_ptrs: SharedStack::new(),
            retired_ptrs: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        self.hzrd_ptrs.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        self.retired_ptrs.lock().unwrap().len()
    }
}

unsafe impl Domain for SharedDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        match self.hzrd_ptrs.iter().find_map(|node| node.try_acquire()) {
            Some(hzrd_ptr) => hzrd_ptr,
            None => self.hzrd_ptrs.push(HzrdPtr::new()),
        }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        self.retired_ptrs.lock().unwrap().push(ret_ptr);
    }

    fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken
        let Ok(mut retired_ptrs) = self.retired_ptrs.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        // SAFETY: We only use this in the domain functions and always drop it
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(self.hzrd_ptrs.iter()) };
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        // Grab the lock to retired pointers
        let mut retired_ptrs = self.retired_ptrs.lock().unwrap();

        // And retire the given pointer
        retired_ptrs.push(ret_ptr);

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        // SAFETY: We only use this in the domain functions and always drop it
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(self.hzrd_ptrs.iter()) };
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
    }
}

// -------------------------------------

use shared_cell::SharedCell;

mod shared_cell {
    pub(crate) struct SharedCell<T>(T);

    impl<T> SharedCell<T> {
        pub(crate) fn new(value: T) -> Self {
            Self(value)
        }

        pub(crate) fn get(&self) -> &T {
            &self.0
        }
    }
}

/// Local, singlethreaded domain
#[derive(Debug)]
pub struct LocalDomain {
    // Important to only allow shared references to the HzrdPtr's
    hzrd_ptrs: UnsafeCell<LinkedList<SharedCell<HzrdPtr>>>,
    retired_ptrs: UnsafeCell<Vec<RetiredPtr>>,
}

impl LocalDomain {
    /**
    Construct a new, clean local domain

    # Example
    ```
    # use hzrd::LocalDomain;
    let domain = LocalDomain::new();
    ```
    */
    pub const fn new() -> Self {
        Self {
            hzrd_ptrs: UnsafeCell::new(LinkedList::new()),
            retired_ptrs: UnsafeCell::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        unsafe { &*self.hzrd_ptrs.get() }.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        unsafe { &*self.retired_ptrs.get() }.len()
    }
}

unsafe impl Domain for LocalDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        {
            let hzrd_ptrs = unsafe { &*self.hzrd_ptrs.get() };

            if let Some(hzrd_ptr) = hzrd_ptrs.iter().find_map(|node| node.get().try_acquire()) {
                return hzrd_ptr;
            }
        }

        let hzrd_ptrs = unsafe { &mut *self.hzrd_ptrs.get() };
        hzrd_ptrs.push_back(SharedCell::new(HzrdPtr::new()));
        unsafe { hzrd_ptrs.back().unwrap_unchecked().get() }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        let retired_ptrs = unsafe { &mut *self.retired_ptrs.get() };
        retired_ptrs.push(ret_ptr);
    }

    fn reclaim(&self) {
        let retired_ptrs = unsafe { &mut *self.retired_ptrs.get() };
        let hzrd_ptrs = unsafe { &*self.hzrd_ptrs.get() };

        // SAFETY: We only use this in the domain functions and always drop it
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(hzrd_ptrs.iter().map(SharedCell::get)) };
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
    }
}

// -------------------------------------

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use super::*;

    fn new_value<T>(value: T) -> NonNull<T> {
        let boxed = Box::new(value);
        let raw = Box::into_raw(boxed);
        unsafe { NonNull::new_unchecked(raw) }
    }

    #[test]
    fn global_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = GlobalDomain;

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(HAZARD_POINTERS.iter()) };
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }

    #[test]
    fn shared_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = SharedDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(domain.hzrd_ptrs.iter()) };
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }

    #[test]
    fn local_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = LocalDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = unsafe { &*domain.hzrd_ptrs.get() };
        let hzrd_ptrs = unsafe { HzrdPtrsCache::load(hzrd_ptrs.iter().map(SharedCell::get)) };
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }
}
