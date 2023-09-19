use std::ptr::NonNull;

use crate::core::{HzrdCore, HzrdPtr, HzrdPtrs};
use crate::linked_list::LinkedList;
use crate::utils::RetiredPtr;
use crate::RefHandle;

pub struct HzrdWriter<T> {
    core: Box<HzrdCore<T>>,
    ptrs: NonNull<Ptrs<T>>,
}

pub struct HzrdReader<'writer, T> {
    core: &'writer HzrdCore<T>,
    hzrd_ptr: NonNull<HzrdPtr>,
}

struct Ptrs<T> {
    hzrd: HzrdPtrs,
    retired: LinkedList<RetiredPtr<T>>,
}

impl<T> HzrdWriter<T> {
    pub fn new(value: T) -> Self {
        Self::from(Box::new(value))
    }

    pub fn reader(&self) -> HzrdReader<T> {
        // SAFETY:
        // - Only the writer can access this
        // - Writer is not Sync, so only this thread can write
        // - This thread is currently doing this
        // - The reference is not held alive beyond this function
        let ptrs = unsafe { &mut *self.ptrs.as_ptr() };

        HzrdReader {
            core: &self.core,
            hzrd_ptr: ptrs.hzrd.get(),
        }
    }

    pub fn set(&self, value: T) {
        let old_ptr = self.core.swap(value);

        // SAFETY:
        // - Only the writer can access this
        // - Writer is not Sync, so only this thread can write
        // - This thread is currently doing this
        // - The reference is not held alive beyond this function
        let ptrs = unsafe { &mut *self.ptrs.as_ptr() };

        ptrs.retired.push_back(RetiredPtr::new(old_ptr));
        crate::utils::reclaim(&mut ptrs.retired, &ptrs.hzrd);
    }
}

impl<T> From<Box<T>> for HzrdWriter<T> {
    fn from(boxed: Box<T>) -> Self {
        let ptrs = Ptrs {
            hzrd: HzrdPtrs::new(),
            retired: LinkedList::new(),
        };

        Self {
            core: Box::new(HzrdCore::new(boxed)),
            ptrs: crate::utils::allocate(ptrs),
        }
    }
}

impl<T> Drop for HzrdWriter<T> {
    fn drop(&mut self) {
        // SAFETY: Noone can access this anymore
        unsafe { crate::utils::free(self.ptrs) }
    }
}

// SAFETY: We good?
unsafe impl<T> Send for HzrdWriter<T> {}

// Private methods
impl<T> HzrdReader<'_, T> {
    fn hzrd_ptr(&self) -> &HzrdPtr {
        // SAFETY: This pointer is valid for as long as this cell is
        unsafe { self.hzrd_ptr.as_ref() }
    }
}

impl<T> HzrdReader<'_, T> {
    pub fn read(reader: &mut Self) -> RefHandle<T> {
        let hzrd_ptr = reader.hzrd_ptr();

        // SAFETY:
        // - We are the owner of the hazard pointer
        // - RefHandle holds exlusive reference via &mut, meaning
        //   no other accesses to hazard pointer before it is dropped
        unsafe { reader.core.read(hzrd_ptr) }
    }
}

impl<T> Drop for HzrdReader<'_, T> {
    fn drop(&mut self) {
        // SAFETY: We won't touch this after this point
        unsafe { self.hzrd_ptr().free() };
    }
}

// SAFETY: We good?
unsafe impl<T> Send for HzrdReader<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fancy() {
        let writer = HzrdWriter::new(0);

        std::thread::scope(|s| {
            let mut reader = writer.reader();
            s.spawn(move || {
                let handle = HzrdReader::read(&mut reader);
                assert!(matches!(*handle, 0 | 1));
            });

            let mut reader = writer.reader();
            s.spawn(move || {
                let handle = HzrdReader::read(&mut reader);
                assert!(matches!(*handle, 0 | 1));
            });

            writer.set(1);

            let mut reader = writer.reader();
            s.spawn(move || {
                let handle = HzrdReader::read(&mut reader);
                assert_eq!(*handle, 1);
            });
        });
    }
}
