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
    pub const fn new() -> Self {
        let null = AtomicPtr::new(std::ptr::null_mut());
        Self { top: null }
    }

    pub fn push(&self, val: T) -> &T {
        let node = Box::into_raw(Box::new(Node::new(val)));
        loop {
            let old_top = self.top.load(Acquire);
            unsafe { &*node }.next.store(old_top, Release);
            if self
                .top
                .compare_exchange(old_top, node, SeqCst, Relaxed)
                .is_ok()
            {
                break;
            }
        }
        unsafe { &(*node).val }
    }

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
            let next = unsafe { (*current).next.load(SeqCst) };
            unsafe { drop(Box::from_raw(current)) };
            current = next;
        }
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
}
