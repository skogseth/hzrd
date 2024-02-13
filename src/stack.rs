use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};

#[derive(Debug)]
pub struct Node<T> {
    val: T,
    next: AtomicPtr<Node<T>>,
}

impl<T> Node<T> {
    pub const fn new(val: T) -> Self {
        let null = AtomicPtr::new(std::ptr::null_mut());
        Self { val, next: null }
    }
}

pub struct SharedStack<T, const READ: bool = true> {
    top: AtomicPtr<Node<T>>,
    count: AtomicUsize,
}

/// Create a new, empty stack
impl<T, const READ: bool> SharedStack<T, READ> {
    pub const fn new() -> Self {
        Self {
            top: AtomicPtr::new(std::ptr::null_mut()),
            count: AtomicUsize::new(0),
        }
    }

    /// Return the current count of the stack
    pub fn count(&self) -> usize {
        // We can use `Relaxed` ordering here since the value
        // will be correctly updated on the previous store
        self.count.load(Relaxed)
    }

    /// Push a new value onto the stack and return a reference to the value
    pub fn push_mut(&mut self, val: T) -> &mut T {
        let node = Box::into_raw(Box::new(Node::new(val)));

        let old_top = self.top.load(SeqCst);
        unsafe { &*node }.next.store(old_top, Release);

        // This should always succeed
        let _exchange_result = self.top.compare_exchange(old_top, node, SeqCst, Relaxed);
        debug_assert!(_exchange_result.is_ok());

        unsafe { &mut (*node).val }
    }
}

impl<T> SharedStack<T> {
    pub fn push(&self, val: T) -> &T {
        let node = Box::into_raw(Box::new(Node::new(val)));

        let mut old_top = self.top.load(Acquire);
        loop {
            // SAFETY: We know that this pointer is valid, we just made it
            unsafe { &*node }.next.store(old_top, Release);

            // We want to exchange the top with our new node, but only if the top is unchanged
            match self.top.compare_exchange(old_top, node, SeqCst, Acquire) {
                // The exchange was successful, the node has been pushed!
                // We can now update the count of the list and exit the loop
                Ok(_) => {
                    // The `Release` ordering here makes the load part of the
                    // operation `Relaxed`, but we don't care about that.
                    self.count.fetch_add(1, Release);
                    break;
                }
                // The value has changed, we update `old_top` to reflect this
                Err(current_top) => old_top = current_top,
            }
        }

        unsafe { &(*node).val }
    }

    /// Push a new value onto the stack and return a reference to the value
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            next: AtomicPtr::new(self.top.load(SeqCst)),
            _marker: PhantomData,
        }
    }

    #[cfg(test)]
    fn to_vec(&self) -> Vec<T>
    where
        T: Copy,
    {
        self.iter().copied().collect()
    }
}

impl<T> SharedStack<T, false> {
    /// Push a new value onto the stack
    pub fn only_push(&self, val: T) {
        let node = Box::into_raw(Box::new(Node::new(val)));

        let mut old_top = self.top.load(Acquire);
        loop {
            // SAFETY: We know that this pointer is valid, we just made it
            unsafe { &*node }.next.store(old_top, Release);

            // We want to exchange the top with our new node, but only if the top is unchanged
            match self.top.compare_exchange(old_top, node, SeqCst, Acquire) {
                // The exchange was successful, the node has been pushed!
                // We can now update the count of the list and exit the loop
                Ok(_) => {
                    // The `Release` ordering here makes the load part of the
                    // operation `Relaxed`, but we don't care about that.
                    self.count.fetch_add(1, Release);
                    break;
                }
                // The value has changed, we update `old_top` to reflect this
                Err(current_top) => old_top = current_top,
            }
        }
    }

    pub fn take(&self) -> Self {
        let top = self.top.swap(std::ptr::null_mut(), SeqCst);
        let count = self.count.swap(0, SeqCst);
        Self {
            top: AtomicPtr::new(top),
            count: AtomicUsize::new(count),
        }
    }
}

impl<T: Debug> Debug for SharedStack<T, true> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T, const READ: bool> Default for SharedStack<T, READ> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const READ: bool> FromIterator<T> for SharedStack<T, READ> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut stack = SharedStack::new();
        for item in iter {
            stack.push_mut(item);
        }
        stack
    }
}

impl<T, const READ: bool> Extend<T> for SharedStack<T, READ> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push_mut(item);
        }
    }
}

impl<T, const READ: bool> IntoIterator for SharedStack<T, READ> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        let next = self.top.load(SeqCst);
        std::mem::forget(self);
        IntoIter { next }
    }
}

impl<'t, T> IntoIterator for &'t SharedStack<T, true> {
    type Item = &'t T;
    type IntoIter = Iter<'t, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, const READ: bool> Drop for SharedStack<T, READ> {
    fn drop(&mut self) {
        let mut current = self.top.load(SeqCst);
        while !current.is_null() {
            let next = unsafe { (*current).next.load(Acquire) };
            unsafe { drop(Box::from_raw(current)) };
            current = next;
        }
    }
}

#[derive(Debug)]
pub struct IntoIter<T> {
    next: *mut Node<T>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next.is_null() {
            return None;
        }

        let current = unsafe { Box::from_raw(self.next) };
        self.next = current.next.load(SeqCst);
        Some(current.val)
    }
}

#[derive(Debug)]
pub struct Iter<'t, T> {
    next: AtomicPtr<Node<T>>,
    _marker: PhantomData<&'t SharedStack<T>>,
}

impl<'t, T> Iterator for Iter<'t, T> {
    type Item = &'t T;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next.load(SeqCst);
        if next.is_null() {
            return None;
        }
        let Node { val, next } = unsafe { &*next };
        let new_next = next.load(SeqCst);
        self.next.store(new_next, SeqCst);
        Some(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> SharedStack<i32> {
        let stack = SharedStack::new();
        stack.push(0);
        stack.push(1);
        stack.push(2);
        stack
    }

    #[test]
    fn stacks_first_test() {
        assert_eq!(stack().to_vec(), [2, 1, 0]);
    }

    #[test]
    fn iter_test() {
        let stack = stack();
        assert_eq!(stack.iter().count(), 3);
    }

    #[test]
    fn multiple_threads() {
        let stack = SharedStack::new();

        std::thread::scope(|s| {
            s.spawn(|| {
                stack.push(1);
                stack.push(2);
            });

            s.spawn(|| {
                stack.push(3);
                stack.push(4);
            });
        });

        assert_eq!(stack.to_vec().len(), 4);
    }

    #[test]
    fn deep_types() {
        let stack = SharedStack::new();

        std::thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..100 {
                    stack.push(vec![String::from("hello"), String::from("worlds")]);
                }
            });

            s.spawn(|| {
                for _ in 0..100 {
                    stack.push(vec![String::from("hazard"), String::from("pointer")]);
                }
            });
        });
    }

    #[test]
    fn iterator() {
        let mut stack: SharedStack<_, true> =
            SharedStack::from_iter([String::from("A"), String::from("B")]);
        stack.extend([String::from("C"), String::from("D")]);
        assert_eq!(Vec::from_iter(stack), ["D", "C", "B", "A"]);
    }
}
