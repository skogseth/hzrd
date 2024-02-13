/*!
Module containing various types implementing the [`Domain`](`crate::core::Domain`)-trait.

The module has three core types:
- [`GlobalDomain`]: A multithreaded, globally shared domain
- [`SharedDomain`]: A multithreaded, shared domain
- [`LocalDomain`]: A singlethreaded, local domain

The default domain used by [`HzrdCell`](`crate::HzrdCell`) is [`GlobalDomain`], which is the recommended domain for most applications.
*/

// -------------------------------------

use std::cell::{Cell, OnceCell, UnsafeCell};
use std::collections::{BTreeSet, LinkedList};
use std::sync::{Mutex, OnceLock};

use crate::core::{Domain, HzrdPtr, RetiredPtr};
use crate::stack::SharedStack;

// -------------------------------------

/*
# Todo:
- Add options for caching:
  -> Maximum size of cache?
  -> Fixed size for cache?
  -> Pre-allocated cache?
- Add option for bulk-reclaim (default to what?), use const generics?
- Test HashSet for cache (BTreeSet can't reuse allocation?)

*/

// -------------------------------------

/**
This variable can be used to configure the behavior of the domains provided by this crate

The variable can only be set once, and this must happen before any operation on any of the domains. If the variable has not been configured before the first access then the default value is used (see [`Config::default`]). The variable uses a standard [`OnceLock`](`std::sync::OnceLock`), and can be used as such:
```
# use hzrd::domains::{Config, GLOBAL_CONFIG};
let config = Config::default().caching(true);
GLOBAL_CONFIG.set(config).unwrap();
```
*/
pub static GLOBAL_CONFIG: OnceLock<Config> = OnceLock::new();

fn global_config() -> &'static Config {
    GLOBAL_CONFIG.get_or_init(Config::default)
}

/**
Config options for domains in this module

If you want to change the global config options then this can be done via [`GLOBAL_CONFIG`]
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Config {
    caching: bool,
    bulk_size: usize,
}

impl Config {
    /// Enable/disable caching (default: `false`)
    pub fn caching(self, caching: bool) -> Self {
        Self { caching, ..self }
    }

    /**
    Set bulk size (default: `1`)

    The bulk size is the smallest amount of elements in a list of retired pointers that will cause memory reclamation to occur. Example: If the bulk size is `4`, then a call to `reclaim` will be a no-op until there is atleast `4` retired objects.

    # Example
    ```
    use hzrd::HzrdCell;
    use hzrd::core::Domain;
    use hzrd::domains::{LocalDomain, Config, GLOBAL_CONFIG};

    let my_config = Config::default().bulk_size(4);
    GLOBAL_CONFIG.set(my_config).unwrap();

    let domain = LocalDomain::new();
    let cell = HzrdCell::new_in(0, &domain);

    // Let's try and update the value a few times
    cell.set(1); // Current garbage: { 0 }
    cell.set(2); // Current garbage: { 0, 1 }
    cell.set(3); // Current garbage: { 0, 1, 2 }

    // This time we will not try to reclaim (we'll do that manually)
    cell.just_set(4); // Current garbage: { 0, 1, 2, 3 }

    // If we now reclaim memory it will reclaim all four
    assert_eq!(domain.reclaim(), 4);
    ```
    */
    pub fn bulk_size(self, bulk_size: usize) -> Self {
        Self { bulk_size, ..self }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            caching: false,
            bulk_size: 1,
        }
    }
}

// -------------------------------------

thread_local! {
    static HAZARD_POINTERS_CACHE: Cell<Vec<usize>> = const { Cell::new(Vec::new()) };
}

/// Holds a loaded set of hazard pointers
struct HzrdPtrs {
    list: Vec<usize>,
    caching: bool,
}

impl HzrdPtrs {
    fn load<'t>(hzrd_ptrs: impl Iterator<Item = &'t HzrdPtr>) -> Self {
        match global_config().caching {
            false => Self::new(hzrd_ptrs),
            true => Self::cached(hzrd_ptrs),
        }
    }

    fn new<'t>(hzrd_ptrs: impl Iterator<Item = &'t HzrdPtr>) -> Self {
        Self {
            list: Vec::from_iter(hzrd_ptrs.map(HzrdPtr::get)),
            caching: false,
        }
    }

    fn cached<'t>(hzrd_ptrs: impl Iterator<Item = &'t HzrdPtr>) -> Self {
        let mut hzrd_ptrs_cache: Vec<usize> = HAZARD_POINTERS_CACHE.with(|cell| cell.take());
        hzrd_ptrs_cache.clear();
        hzrd_ptrs_cache.extend(hzrd_ptrs.map(HzrdPtr::get));

        Self {
            list: hzrd_ptrs_cache,
            caching: true,
        }
    }

    fn contains(&self, addr: usize) -> bool {
        self.list.contains(&addr)
    }
}

/**
If the hazard pointers were loaded using the cache we'll return the cache

If the cache is loaded twice in overlap then only the first will get a cache-hit.
The second load will then need to allocate all memory needed.
The cache will be overwritten by the last to access it.
*/
impl Drop for HzrdPtrs {
    fn drop(&mut self) {
        if self.caching {
            let list = std::mem::take(&mut self.list);
            HAZARD_POINTERS_CACHE.with(|cell| cell.set(list));
        }
    }
}

// -------------------------------------

static HAZARD_POINTERS: SharedStack<HzrdPtr> = SharedStack::new();

static SHARED_RETIRED_POINTERS: Mutex<Vec<RetiredPtr>> = Mutex::new(Vec::new());

thread_local! {
    static LOCAL_RETIRED_POINTERS: LocalRetiredPtrs = const { LocalRetiredPtrs(UnsafeCell::new(Vec::new())) };
}

/// We need a special wrapper type to handle cleanup on closing threads.
struct LocalRetiredPtrs(UnsafeCell<Vec<RetiredPtr>>);

/**
There is a potential for the drop-function to not be called for thread-locals
(see https://doc.rust-lang.org/std/thread/struct.LocalKey.html).
This should, in our case, only lead to memory leaks, which is okay because it seems that the occasions in which the drop-function is (potentially) not called is close to program exit.
*/
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

This is the default domain used by `HzrdCell`, and is the recommended domain for most applications. It's based on a set of globally shared, static variables, and so there is no "constructor" for this domain. The [`GlobalDomain`] struct is  a Zero Sized Type (ZST) that acts simply as an accessor to these globally shared variables.

# Example
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
// Both of which are at the moment in active use (in `handle_1` and `handle_2`, respectively)

// We write some values to the cells, which will not be able to free the previous
// values in the cell as there are references to these in `handle_1` and `handle_2`
cell_1.set(1);
cell_2.set(true);

// The `GlobalDomain` now has the following garbage: (0, false)

// Drop both handles, so garbage can (eventually) be freed
drop(_handle_1);
drop(_handle_2);

// Free all garbage in the `GlobalDomain`
cell_1.reclaim();

// There is no need to call `HzrdCell::reclaim` on cell_2 as they both share the `GlobalDomain`.
```

# Thread local garbage
There is some more complexity to the garbage collection in `GlobalDomain`: Each thread holds its own local garbage, as well as access to some garbage shared between all threads. If a thread closes down with local garbage still remaining (it will attempt one last cleanup before closing), then that garbage will be moved to the shared garbage. Whenever a thread does garbage collection it will first try to clean up the local garbage it holds, followed by an attempt to clean up the shared garbage. However, since the shared garbage is locked by a [`Mutex`](`std::sync::Mutex`) it will only attempt to do soM; if the shared garbage is locked by another thread this step will be skipped.
*/
#[derive(Clone, Copy)]
pub struct GlobalDomain;

impl GlobalDomain {
    #[cfg(test)]
    pub(crate) fn number_of_hzrd_ptrs(&self) -> usize {
        HAZARD_POINTERS.count()
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        LOCAL_RETIRED_POINTERS.with(|cell| unsafe { (*cell.0.get()).len() })
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

    fn reclaim(&self) -> usize {
        // Lazily load hazard pointers (waiting for `LazyCell` to stabilize...)
        let hzrd_ptrs = OnceCell::new();
        let hzrd_ptrs = || hzrd_ptrs.get_or_init(|| HzrdPtrs::load(HAZARD_POINTERS.iter()));

        let locally_reclaimed = LOCAL_RETIRED_POINTERS.with(|cell| {
            let retired_ptrs = unsafe { &mut *cell.0.get() };
            let prev_size = retired_ptrs.len();

            if prev_size < global_config().bulk_size {
                return 0;
            }

            retired_ptrs.retain(|p| hzrd_ptrs().contains(p.addr()));
            prev_size - retired_ptrs.len()
        });

        let shared_reclaimed = match SHARED_RETIRED_POINTERS.try_lock() {
            Ok(mut retired_ptrs) => {
                let prev_size = retired_ptrs.len();

                if prev_size < global_config().bulk_size {
                    return 0;
                }

                retired_ptrs.retain(|p| hzrd_ptrs().contains(p.addr()));
                prev_size - retired_ptrs.len()
            }
            Err(_) => 0,
        };

        locally_reclaimed + shared_reclaimed
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

/**
Shared, multithreaded domain

A shared domain can, in contrast to [`GlobalDomain`], be owned by the [`HzrdCell`](`crate::HzrdCell`) itself. This means the cell will hold exclusive access to it, and the garbage associated with the domain will be cleaned up when the cell is dropped. This can be abused to delay all garbage collection for some limited time in order to do it all in bulk:

```
use hzrd::{HzrdCell, SharedDomain};

let cell = HzrdCell::new_in(0, SharedDomain::new());

std::thread::scope(|s| {
    s.spawn(|| {
        // Let's see how quickly we can count to thirty
        for i in 0..30 {
            // Intentionally avoid all garbage collection
            cell.just_set(i);
        }

        // We have finished counting, now we clean up
        cell.reclaim();
    });

    s.spawn(|| {
        println!("Let's check what the value is! {}", cell.get());
    });
});
```

Another interesting option with [`SharedDomain`] is to have the domain stored in an [`Arc`](`std::sync::Arc`). Multiple cells can now share a single domain, but that domain (including all the associated garbage) will still be guaranteed to be cleaned up when all the cells are dropped.
```
use std::sync::Arc;

use hzrd::{HzrdCell, SharedDomain};

let custom_domain = Arc::new(SharedDomain::new());
let cell_1 = HzrdCell::new_in(0, Arc::clone(&custom_domain));
let cell_2 = HzrdCell::new_in(false, Arc::clone(&custom_domain));
# assert_eq!(cell_1.get(), 0);
# assert_eq!(cell_2.get(), false);
```
*/
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
        self.hzrd_ptrs.count()
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

    fn reclaim(&self) -> usize {
        // Try to aquire lock, exit if it is taken
        let Ok(mut retired_ptrs) = self.retired_ptrs.try_lock() else {
            return 0;
        };

        let prev_size = retired_ptrs.len();

        // Check if it's too small
        if prev_size < global_config().bulk_size {
            return 0;
        }

        let hzrd_ptrs = HzrdPtrs::load(self.hzrd_ptrs.iter());
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        prev_size - retired_ptrs.len()
    }

    // -------------------------------------

    // Override this for (hopefully) improved performance
    fn retire(&self, ret_ptr: RetiredPtr) -> usize {
        // Grab the lock to retired pointers
        let mut retired_ptrs = self.retired_ptrs.lock().unwrap();

        // And retire the given pointer
        retired_ptrs.push(ret_ptr);

        let prev_size = retired_ptrs.len();

        // Check if it's too small
        if prev_size < global_config().bulk_size {
            return 0;
        }

        let hzrd_ptrs = HzrdPtrs::load(self.hzrd_ptrs.iter());
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        prev_size - retired_ptrs.len()
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

/**
Local, singlethreaded domain

The main use case for this is when only a single thread needs to be able to write to a cell. Since the `Domain` is not `Sync` the `HzrdCell` constructed with it won't be either, as this requires both the value held and the domain to be thread-safe. However, `HzrdReader` holds no access to the domain, only a reference to the value. It is therefore `Send` if and only if the value held is both `Send` and `Sync`. Using this we can create a single-writer, multiple-readers construct.

# Example
```
use std::sync::Barrier;

use hzrd::{HzrdCell, LocalDomain};

const N_THREADS: usize = 10;

let cell = HzrdCell::new_in(0, LocalDomain::new());
let barrier = Barrier::new(N_THREADS + 1);

// We use scoped threads to avoid requirements for 'static lifetimes
std::thread::scope(|s| {
    for i in 0..N_THREADS  {
        // We need to construct readers, as the cell is not `Sync`
        let mut reader = cell.reader();

        // Borrow this, so as to not move it into the thread
        let barrier = &barrier;

        // We now send the reader to the spawned thread
        s.spawn(move || {
            // Wait for everyone to be ready
            barrier.wait();

            // All threads read at the same time!
            println!("[{i}]: {}", reader.get());
        });
    }

    // Wait for all the threads to be ready
    barrier.wait();

    // Then start counting
    for num in 1..5 {
        // Don't perform garbage collection
        cell.just_set(num);
    }

    // We're done, but no need to clean up...
});

// The cell is dropped, all garbage is cleaned up
drop(cell);
```
*/
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
        unsafe { (*self.hzrd_ptrs.get()).len() }
    }

    #[cfg(test)]
    pub(crate) fn number_of_retired_ptrs(&self) -> usize {
        unsafe { (*self.retired_ptrs.get()).len() }
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

    fn reclaim(&self) -> usize {
        let retired_ptrs = unsafe { &mut *self.retired_ptrs.get() };
        let hzrd_ptrs = unsafe { &mut *self.hzrd_ptrs.get() };

        let prev_size = retired_ptrs.len();

        // Check if it's too small
        if prev_size < global_config().bulk_size {
            return 0;
        }

        let hzrd_ptrs = HzrdPtrs::load(hzrd_ptrs.iter().map(SharedCell::get));
        retired_ptrs.retain(|p| hzrd_ptrs.contains(p.addr()));
        prev_size - retired_ptrs.len()
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

    #[ignore]
    #[test]
    fn global_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = GlobalDomain;

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = HzrdPtrs::load(HAZARD_POINTERS.iter());
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        // Retire the pointer. Nothing should be reclaimed this time
        {
            let reclaimed = domain.retire(unsafe { RetiredPtr::new(ptr) });
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // Nothing has changed with the hazard pointer, nothing will be reclaimed
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // We now reset the hazard pointer and try again
        unsafe { hzrd_ptr.reset() };

        // This time there should be one reclaimed object, zero left
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 1);
            assert_eq!(domain.number_of_retired_ptrs(), 0);
        }
    }

    #[test]
    fn shared_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = SharedDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = HzrdPtrs::load(domain.hzrd_ptrs.iter());
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        // Retire the pointer. Nothing should be reclaimed this time
        {
            let reclaimed = domain.retire(unsafe { RetiredPtr::new(ptr) });
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // Nothing has changed with the hazard pointer, nothing will be reclaimed
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // We now reset the hazard pointer and try again
        unsafe { hzrd_ptr.reset() };

        // This time there should be one reclaimed object, zero left
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 1);
            assert_eq!(domain.number_of_retired_ptrs(), 0);
        }
    }

    #[test]
    fn local_domain() {
        let ptr = new_value(['a', 'b', 'c', 'd']);
        let domain = LocalDomain::new();

        let hzrd_ptr = domain.hzrd_ptr();
        assert_eq!(domain.number_of_hzrd_ptrs(), 1);

        unsafe { hzrd_ptr.protect(ptr.as_ptr()) };
        let hzrd_ptrs = unsafe { &*domain.hzrd_ptrs.get() };
        let hzrd_ptrs = HzrdPtrs::load(hzrd_ptrs.iter().map(SharedCell::get));
        assert!(hzrd_ptrs.contains(ptr.as_ptr() as usize));

        // Retire the pointer. Nothing should be reclaimed this time
        {
            let reclaimed = domain.retire(unsafe { RetiredPtr::new(ptr) });
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // Nothing has changed with the hazard pointer, nothing will be reclaimed
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 0);
            assert_eq!(domain.number_of_retired_ptrs(), 1);
        }

        // We now reset the hazard pointer and try again
        unsafe { hzrd_ptr.reset() };

        // This time there should be one reclaimed object, zero left
        {
            let reclaimed = domain.reclaim();
            assert_eq!(reclaimed, 1);
            assert_eq!(domain.number_of_retired_ptrs(), 0);
        }
    }
}
