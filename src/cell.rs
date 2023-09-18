use std::ptr::NonNull;
use std::sync::Mutex;

use crate::core::HzrdCore;
use crate::linked_list::LinkedList;
use crate::ptr::{HzrdPtr, HzrdPtrs};
use crate::utils::RetiredPtr;

/// Shared heap allocated object for `HzrdCell`
///
/// The `hzrd_ptrs` keep track of pointers that are in use, and cannot be freed
/// There is one node per `HzrdCell`, which means the list also keeps track
/// of the number of active `HzrdCell`s (akin to a very inefficent atomic counter).
pub struct HzrdCellInner<T> {
    pub core: HzrdCore<T>,
    pub hzrd_ptrs: Mutex<HzrdPtrs>,
    pub retired: Mutex<LinkedList<RetiredPtr<T>>>,
}

impl<T> HzrdCellInner<T> {
    pub fn new(boxed: Box<T>) -> Self {
        Self {
            core: HzrdCore::new(boxed),
            hzrd_ptrs: Mutex::new(HzrdPtrs::new()),
            retired: Mutex::new(LinkedList::new()),
        }
    }

    pub fn add(&self) -> NonNull<HzrdPtr> {
        self.hzrd_ptrs.lock().unwrap().get()
    }

    /// Reclaim available memory
    pub fn reclaim(&self) {
        // Try to aquire lock, exit if it is taken, as this
        // means someone else is already reclaiming memory!
        let Ok(mut retired) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired.is_empty() {
            return;
        }

        // Wait for access to the hazard pointers
        let hzrd_ptrs = self.hzrd_ptrs.lock().unwrap();

        crate::utils::reclaim(&mut retired, &hzrd_ptrs);
    }

    /// Try to reclaim memory, but don't wait for the shared lock to do so
    pub fn try_reclaim(&self) {
        // Try to aquire lock, exit if it is taken, as this
        // means someone else is already reclaiming memory!
        let Ok(mut retired) = self.retired.try_lock() else {
            return;
        };

        // Check if it's empty, no need to move forward otherwise
        if retired.is_empty() {
            return;
        }

        // Check if the hazard pointers are available, if not exit
        let Ok(hzrd_ptrs) = self.hzrd_ptrs.try_lock() else {
            return;
        };

        crate::utils::reclaim(&mut retired, &hzrd_ptrs);
    }
}
