#![allow(dead_code)]

//! Implementation of linked list using raw pointers

use std::marker::PhantomData;
use std::ptr::null_mut;

#[derive(Debug)]
pub struct LinkedList<T> {
    head: *mut Node<T>,
    tail: *mut Node<T>,
}

#[derive(Debug)]
pub struct Node<T> {
    value: T,
    next: *mut Node<T>,
    prev: *mut Node<T>,
}

impl<T> Node<T> {
    pub unsafe fn get_from_raw(ptr: *mut Self) -> *mut T {
        &mut (*ptr).value
    }
}

/// Does not clean up allocated memory!
fn allocate<T>(value: T) -> *mut T {
    let ptr = Box::new(value);
    Box::into_raw(ptr)
}

impl<T> LinkedList<T> {
    pub fn new() -> LinkedList<T> {
        LinkedList {
            head: null_mut(),
            tail: null_mut(),
        }
    }

    pub fn single(value: T) -> LinkedList<T> {
        let node = Node {
            value,
            next: null_mut(),
            prev: null_mut(),
        };
        let ptr = allocate(node);
        LinkedList {
            head: ptr,
            tail: ptr,
        }
    }

    pub fn push_front(&mut self, value: T) {
        if self.head.is_null() {
            *self = LinkedList::single(value);
            return;
        }

        let node = Node {
            value,
            next: self.head,
            prev: null_mut(),
        };
        let ptr = allocate(node);
        unsafe { (*self.head).prev = ptr };
        self.head = ptr;
    }

    pub fn push_back(&mut self, value: T) {
        if self.tail.is_null() {
            *self = LinkedList::single(value);
            return;
        }

        let node = Node {
            value,
            next: null_mut(),
            prev: self.tail,
        };
        let ptr = allocate(node);
        unsafe { (*self.tail).next = ptr };
        self.tail = ptr;
    }

    pub fn pop_front(&mut self) -> Option<T> {
        if self.head.is_null() {
            return None;
        }

        // SAFETY: Can never access self.head after this!
        let Node {
            value,
            next: second,
            ..
        } = unsafe { *Box::from_raw(self.head) };

        if second.is_null() {
            self.head = null_mut();
            self.tail = null_mut();
        } else {
            unsafe { (*second).prev = null_mut() };
            self.head = second;
        }

        Some(value)
    }

    pub fn pop_back(&mut self) -> Option<T> {
        if self.tail.is_null() {
            return None;
        }

        // Can never access self.tail after this!
        let Node {
            value,
            prev: penultimate,
            ..
        } = unsafe { *Box::from_raw(self.tail) };

        if penultimate.is_null() {
            self.head = null_mut();
            self.tail = null_mut();
        } else {
            unsafe { (*penultimate).next = null_mut() };
            self.tail = penultimate;
        }

        Some(value)
    }

    /// SAFETY: The node pointer must point to a node in the given `LinkedList`
    pub unsafe fn remove_node(&mut self, ptr: *mut Node<T>) -> T {
        // SAFETY: Cannot access ptr after this
        let boxed = unsafe { Box::from_raw(ptr) };
        let Node { next, prev, value } = *boxed;

        if prev.is_null() {
            self.head = next;
        } else {
            (*prev).next = next;
        };

        if next.is_null() {
            self.tail = prev;
        } else {
            (*next).prev = prev;
        }

        value
    }

    pub fn single_and_get_raw(value: T) -> (LinkedList<T>, *mut Node<T>) {
        let node = Node {
            value,
            next: null_mut(),
            prev: null_mut(),
        };
        let ptr = allocate(node);
        let list = LinkedList {
            head: ptr,
            tail: ptr,
        };
        (list, ptr)
    }

    pub fn push_front_and_get_raw(&mut self, value: T) -> *mut Node<T> {
        if self.head.is_null() {
            let (list, ptr) = LinkedList::single_and_get_raw(value);
            *self = list;
            return ptr;
        }

        let node = Node {
            value,
            next: self.head,
            prev: null_mut(),
        };
        let ptr = allocate(node);
        unsafe { (*self.head).prev = ptr };
        self.head = ptr;
        ptr
    }

    pub fn push_back_and_get_raw(&mut self, value: T) -> *mut Node<T> {
        if self.tail.is_null() {
            let (list, ptr) = LinkedList::single_and_get_raw(value);
            *self = list;
            return ptr;
        }

        let node = Node {
            value,
            next: null_mut(),
            prev: self.tail,
        };
        let ptr = allocate(node);
        unsafe { (*self.tail).next = ptr };
        self.tail = ptr;
        ptr
    }

    pub fn is_empty(&self) -> bool {
        debug_assert!(self.head.is_null() == self.tail.is_null());
        self.head.is_null()
    }

    pub fn iter(&mut self) -> Iter<T> {
        Iter {
            head: self.head,
            tail: self.tail,
            marker: PhantomData,
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<T> {
        IterMut {
            head: self.head,
            tail: self.tail,
            marker: PhantomData,
        }
    }
}

impl<T> Default for LinkedList<T> {
    fn default() -> Self {
        LinkedList::new()
    }
}

impl<T> From<Vec<T>> for LinkedList<T> {
    fn from(vec: Vec<T>) -> Self {
        let mut list = LinkedList::new();
        for elem in vec {
            list.push_back(elem);
        }
        list
    }
}

impl<T, const N: usize> From<[T; N]> for LinkedList<T> {
    fn from(arr: [T; N]) -> Self {
        let mut list = LinkedList::new();
        for elem in arr {
            list.push_back(elem);
        }
        list
    }
}

impl<T> Drop for LinkedList<T> {
    fn drop(&mut self) {
        while self.pop_back().is_some() {}
    }
}

impl<T> Iterator for LinkedList<T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.pop_front()
    }
}

pub struct Iter<'a, T> {
    head: *mut Node<T>,
    tail: *mut Node<T>,
    marker: PhantomData<&'a T>,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.head.is_null() {
            let node = self.head;
            unsafe {
                self.head = (*node).next;
                Some(&(*node).value)
            }
        } else {
            None
        }
    }
}

impl<'a, T> DoubleEndedIterator for Iter<'a, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if !self.tail.is_null() {
            let node = self.tail;
            unsafe {
                self.tail = (*node).prev;
                Some(&(*node).value)
            }
        } else {
            None
        }
    }
}

pub struct IterMut<'a, T> {
    head: *mut Node<T>,
    tail: *mut Node<T>,
    marker: PhantomData<&'a mut T>,
}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.head.is_null() {
            let node = self.head;
            unsafe {
                self.head = (*node).next;
                Some(&mut (*node).value)
            }
        } else {
            None
        }
    }
}

impl<'a, T> DoubleEndedIterator for IterMut<'a, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if !self.tail.is_null() {
            let node = self.tail;
            unsafe {
                self.tail = (*node).prev;
                Some(&mut (*node).value)
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_empty() {
        let list: LinkedList<i32> = LinkedList::new();
        assert!(list.is_empty());
    }

    #[test]
    fn list_i32() {
        let mut list = LinkedList::new();
        list.push_back(0);
        list.push_back(1);
        assert_eq!(list.pop_back(), Some(1));
        assert_eq!(list.pop_back(), Some(0));
        assert_eq!(list.pop_back(), None);
    }

    #[test]
    fn list_from_arr() {
        let mut list = LinkedList::from([1, 2, 3]);
        assert_eq!(list.pop_back(), Some(3));
        assert_eq!(list.pop_back(), Some(2));
        assert_eq!(list.pop_back(), Some(1));
        assert_eq!(list.pop_back(), None);
    }

    #[test]
    fn list_from_vec() {
        let vec = vec![1, 2, 3, 4, 5];
        let mut list = LinkedList::from(vec);
        assert_eq!(list.pop_back(), Some(5));
        assert_eq!(list.pop_back(), Some(4));
        assert_eq!(list.pop_back(), Some(3));
        assert_eq!(list.pop_back(), Some(2));
        assert_eq!(list.pop_back(), Some(1));
        assert_eq!(list.pop_back(), None);
    }

    #[test]
    fn frees() {
        let vec = vec![1, 2, 3, 4, 5];
        let _ = LinkedList::from(vec);
    }
}
