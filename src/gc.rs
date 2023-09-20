use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::RwLock;

use crate::core::{HzrdCore, HzrdPtr, HzrdPtrs};
use crate::linked_list::LinkedList;
use crate::utils::RetiredPtr;

pub struct HzrdWriter<T> {
    inner: NonNull<Inner<T>>,
    // Field for RwLockWriteGuard?
}

pub struct HzrdReader<T> {
    inner: NonNull<Inner<T>>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

struct Inner<T> {
    core: HzrdCore<T>,
    ptrs: Ptrs<T>,
    writer: AtomicBool,
    guard: RwLock<()>,
}

struct Ptrs<T> {
    hzrd: HzrdPtrs,
    retired: LinkedList<RetiredPtr<T>>,
}

impl<T> HzrdWriter<T> {
    pub fn new(value: T) -> Self {
        Self::from(Box::new(value))
    }
}

impl<T> From<Box<T>> for HzrdWriter<T> {
    fn from(boxed: Box<T>) -> Self {
        let ptrs = Ptrs {
            hzrd: HzrdPtrs::new(),
            retired: LinkedList::new(),
        };

        let inner = Inner {
            core: HzrdCore::new(boxed),
            ptrs,
        };

        Self {
            inner: Arc::new(inner),
        }
    }
}

impl<T> Drop for HzrdReader<T> {
    fn drop(&mut self) {
        // SAFETY: This is our HzrdPtr!!
        unsafe { self.hzrd_ptr.as_ref().free() };
    }
}
