use std::sync::atomic::{AtomicPtr, Ordering::*};

pub struct Node<T> {
    val: T,
    next: AtomicPtr<Node<T>>,
}

impl<T> Node<T> {
    pub fn new(val: T) -> Self {
        let null = AtomicPtr::new(std::ptr::null_mut());
        Self { val, next: null }
    }
}

pub struct SharedStack<T> {
    top: AtomicPtr<Node<T>>,
}

impl<T> SharedStack<T> {
    pub fn new() -> Self {
        let null = AtomicPtr::new(std::ptr::null_mut());
        Self { top: null }
    }

    pub fn push(&self, val: T) {
        let top = &self.top;
        let node = Box::into_raw(Box::new(Node::new(val)));
        loop {
            let old_top = top.load(Acquire);
            unsafe { &*node }.next.store(old_top, Release);
            if top.compare_exchange(old_top, node, SeqCst, SeqCst).is_ok() {
                break;
            }
        }
    }
}

impl<T> Default for SharedStack<T> {
    fn default() -> Self {
        Self::new()
    }
}
