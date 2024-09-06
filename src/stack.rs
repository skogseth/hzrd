use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicPtr, Ordering::*};

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

pub struct SharedStack<T> {
    top: AtomicPtr<Node<T>>,
}

impl<T> SharedStack<T> {
    /// Create a new, empty stack
    pub const fn new() -> Self {
        Self {
            top: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn __push(&self, node: *mut Node<T>) {
        std::sync::atomic::fence(SeqCst);

        let mut old_top = self.top.load(Acquire);
        loop {
            // SAFETY: We know that this pointer is valid, we just made it
            unsafe { &*node }.next.store(old_top, Release);

            // We want to exchange the top with our new node, but only if the top is unchanged
            match self.top.compare_exchange(old_top, node, AcqRel, Acquire) {
                // The exchange was successful, the node has been pushed!
                // We can now update the count of the list and exit the loop
                Ok(_) => break,
                // The value has changed, we update `old_top` to reflect this
                Err(current_top) => old_top = current_top,
            }
        }
    }

    /// Push a new value onto the stack
    pub fn push(&self, val: T) {
        let node = Box::into_raw(Box::new(Node::new(val)));
        let _ = self.__push(node);
    }

    /// Push a new value onto the stack and return a reference to the value
    pub fn push_get(&self, val: T) -> &T {
        let node = Box::into_raw(Box::new(Node::new(val)));
        self.__push(node);
        unsafe { &(*node).val }
    }

    /// Push a new value onto the stack and return a mutable reference to the value
    pub fn push_mut(&mut self, val: T) {
        let node = Box::into_raw(Box::new(Node::new(val)));

        let old_top = self.top.load(Acquire);
        unsafe { &*node }.next.store(old_top, Release);

        // This should always succeed
        let _exchange_result = self.top.compare_exchange(old_top, node, SeqCst, Relaxed);
        debug_assert!(_exchange_result.is_ok());
    }

    pub fn push_stack(&self, stack: Self) {
        // TODO: This can be done much more efficiently
        for val in stack {
            let node = Box::into_raw(Box::new(Node::new(val)));
            let _ = self.__push(node);
        }
    }

    pub unsafe fn take(&self) -> Self {
        std::sync::atomic::fence(SeqCst);
        let top = self.top.swap(std::ptr::null_mut(), Acquire);
        Self {
            top: AtomicPtr::new(top),
        }
    }

    /// Create an iterator over the stack
    pub fn iter(&self) -> Iter<'_, T> {
        std::sync::atomic::fence(SeqCst);
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

impl<T: Debug> Debug for SharedStack<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T> Default for SharedStack<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> FromIterator<T> for SharedStack<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut stack = SharedStack::new();
        for item in iter {
            stack.push_mut(item);
        }
        stack
    }
}

impl<T> Extend<T> for SharedStack<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push_mut(item);
        }
    }
}

impl<T> IntoIterator for SharedStack<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        let next = self.top.load(SeqCst);
        std::mem::forget(self);
        IntoIter { next }
    }
}

impl<'t, T> IntoIterator for &'t SharedStack<T> {
    type Item = &'t T;
    type IntoIter = Iter<'t, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> Drop for SharedStack<T> {
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
        let next = self.next.load(Acquire);
        if next.is_null() {
            return None;
        }
        let Node { val, next } = unsafe { &*next };
        let new_next = next.load(Acquire);
        self.next.store(new_next, Release);
        Some(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> SharedStack<i32> {
        let stack = SharedStack::new();
        stack.push_get(0);
        stack.push_get(1);
        stack.push_get(2);
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
                stack.push_get(1);
                stack.push_get(2);
            });

            s.spawn(|| {
                stack.push_get(3);
                stack.push_get(4);
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
                    stack.push_get(vec![String::from("hello"), String::from("worlds")]);
                }
            });

            s.spawn(|| {
                for _ in 0..100 {
                    stack.push_get(vec![String::from("hazard"), String::from("pointer")]);
                }
            });
        });
    }

    #[test]
    fn iterator() {
        let mut stack = SharedStack::from_iter([String::from("A"), String::from("B")]);
        stack.extend([String::from("C"), String::from("D")]);
        assert_eq!(Vec::from_iter(stack), ["D", "C", "B", "A"]);
    }
}
