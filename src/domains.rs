use std::cell::UnsafeCell;
use std::collections::{BTreeSet, LinkedList};
use std::sync::Mutex;

use crate::core::{Domain, HzrdPtr, RetiredPtr};
use crate::stack::SharedStack;

// -------------------------------------

static HAZARD_POINTERS: SharedStack<HzrdPtr> = SharedStack::new();

static SHARED_RETIRED_POINTERS: Mutex<Vec<RetiredPtr>> = Mutex::new(Vec::new());

thread_local! {
    static LOCAL_RETIRED_POINTERS: LocalRetiredPtrs = const { LocalRetiredPtrs(UnsafeCell::new(Vec::new())) };

    static HAZARD_POINTERS_CACHE: UnsafeCell<Vec<usize>> = const { UnsafeCell::new(Vec::new()) };
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

/// Globally shared, multithreaded domain
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
        HAZARD_POINTERS_CACHE.with(|cell| {
            let hzrd_ptrs = unsafe { &mut *cell.get() };
            hzrd_ptrs.clear();
            hzrd_ptrs.extend(HAZARD_POINTERS.iter().map(HzrdPtr::get));

            LOCAL_RETIRED_POINTERS.with(|cell| {
                let retired_ptrs = unsafe { &mut *cell.0.get() };
                retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
            });

            if let Ok(mut retired_ptrs) = SHARED_RETIRED_POINTERS.try_lock() {
                retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
            }
        });
    }

    // -------------------------------------

    // Override this to avoid mutable aliasing
    fn retire(&self, ret_ptr: RetiredPtr) {
        HAZARD_POINTERS_CACHE.with(|cell| {
            let hzrd_ptrs = unsafe { &mut *cell.get() };
            hzrd_ptrs.clear();
            hzrd_ptrs.extend(HAZARD_POINTERS.iter().map(HzrdPtr::get));

            LOCAL_RETIRED_POINTERS.with(|cell| {
                let retired_ptrs = unsafe { &mut *cell.0.get() };
                retired_ptrs.push(ret_ptr);
                retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
            });

            if let Ok(mut retired_ptrs) = SHARED_RETIRED_POINTERS.try_lock() {
                retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
            }
        });
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
    hzrd: SharedStack<HzrdPtr>,
    retired: Mutex<Vec<RetiredPtr>>,
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
            hzrd: SharedStack::new(),
            retired: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        self.hzrd.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        self.retired.lock().unwrap().len()
    }
}

unsafe impl Domain for SharedDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        // Important to only grab shared references to the HzrdPtr's
        // as others may be looking at them
        match self.hzrd.iter().find_map(|node| node.try_acquire()) {
            Some(hzrd_ptr) => hzrd_ptr,
            None => self.hzrd.push(HzrdPtr::new()),
        }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        self.retired.lock().unwrap().push(ret_ptr);
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

        let hzrd_ptrs: BTreeSet<_> = self.hzrd.iter().map(HzrdPtr::get).collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        // Grab the lock to retired pointers
        let mut retired_ptrs = self.retired.lock().unwrap();

        // And retire the given pointer
        retired_ptrs.push(ret_ptr);

        // Check if it's empty, no need to move forward otherwise
        if retired_ptrs.is_empty() {
            return;
        }

        let hzrd_ptrs: BTreeSet<_> = self.hzrd.iter().map(HzrdPtr::get).collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
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
    hzrd: UnsafeCell<LinkedList<SharedCell<HzrdPtr>>>,
    retired: UnsafeCell<Vec<RetiredPtr>>,
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
            hzrd: UnsafeCell::new(LinkedList::new()),
            retired: UnsafeCell::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        unsafe { &*self.hzrd.get() }.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        unsafe { &*self.retired.get() }.len()
    }
}

unsafe impl Domain for LocalDomain {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        {
            let hzrd_ptrs = unsafe { &*self.hzrd.get() };

            if let Some(hzrd_ptr) = hzrd_ptrs.iter().find_map(|node| node.get().try_acquire()) {
                return hzrd_ptr;
            }
        }

        let hzrd_ptrs = unsafe { &mut *self.hzrd.get() };
        hzrd_ptrs.push_back(SharedCell::new(HzrdPtr::new()));
        unsafe { hzrd_ptrs.back().unwrap_unchecked().get() }
    }

    fn just_retire(&self, ret_ptr: RetiredPtr) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        retired_ptrs.push(ret_ptr);
    }

    fn reclaim(&self) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        let hzrd_ptrs = unsafe { &*self.hzrd.get() };

        let hzrd_ptrs: BTreeSet<_> = hzrd_ptrs
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) {
        let retired_ptrs = unsafe { &mut *self.retired.get() };
        let hzrd_ptrs = unsafe { &*self.hzrd.get() };

        retired_ptrs.push(ret_ptr);

        let hzrd_ptrs: BTreeSet<_> = hzrd_ptrs
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        retired_ptrs.retain(|p| hzrd_ptrs.contains(&p.addr()));
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
        let set: BTreeSet<_> = HAZARD_POINTERS.iter().map(HzrdPtr::get).collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

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
        let set: BTreeSet<_> = domain.hzrd.iter().map(HzrdPtr::get).collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

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
        let set: BTreeSet<_> = unsafe { &*domain.hzrd.get() }
            .iter()
            .map(SharedCell::get)
            .map(HzrdPtr::get)
            .collect();
        assert!(set.contains(&(ptr.as_ptr() as usize)));

        domain.retire(unsafe { RetiredPtr::new(ptr) });
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 1);

        unsafe { hzrd_ptr.reset() };

        domain.reclaim();
        assert_eq!(domain.number_of_retired_ptrs(), 0);
    }
}
