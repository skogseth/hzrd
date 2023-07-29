#![allow(dead_code)]

//! Implementation of linked list using raw pointers

use std::marker::PhantomData;
use std::ptr::{null_mut, NonNull};

#[derive(Debug)]
pub struct LinkedList<T> {
    head: Option<NonNull<Node<T>>>,
    tail: Option<NonNull<Node<T>>>,
}

#[derive(Debug)]
pub struct Node<T> {
    value: T,
    next: Option<NonNull<Node<T>>>,
    prev: Option<NonNull<Node<T>>>,
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
            head: None,
            tail: None,
        }
    }

    pub fn single(value: T) -> LinkedList<T> {
        let node = Node {
            value,
            next: None,
            prev: None,
        };
        let ptr = crate::utils::allocate(node);
        LinkedList {
            head: Some(ptr),
            tail: Some(ptr),
        }
    }

    pub fn push_front(&mut self, value: T) {
        let Some(head) = self.head else {
            *self = LinkedList::single(value);
            return;
        };

        let node = Node {
            value,
            next: Some(head),
            prev: None,
        };
        let ptr = crate::utils::allocate(node);
        unsafe { (*head.as_ptr()).prev = Some(ptr) };
        self.head = Some(ptr);
    }

    pub fn push_back(&mut self, value: T) {
        let Some(tail) = self.tail else {
            *self = LinkedList::single(value);
            return;
        };

        let node = Node {
            value,
            next: None,
            prev: Some(tail),
        };
        let ptr = crate::utils::allocate(node);
        unsafe { (*tail.as_ptr()).next = Some(ptr) };
        self.tail = Some(ptr);
    }

    pub fn pop_front(&mut self) -> Option<T> {
        let Some(head) = self.head else {
            return None;
        };

        // SAFETY: Can never access self.head after this!
        let Node {
            value,
            next: second,
            ..
        } = unsafe { *Box::from_raw(head.as_ptr()) };

        if let Some(second) = second {
            unsafe { (*second.as_ptr()).prev = None };
            self.head = Some(second);
        } else {
            self.head = None;
            self.tail = None;
        }

        Some(value)
    }

    pub fn pop_back(&mut self) -> Option<T> {
        let Some(tail) = self.tail else {
            return None;  
        };

        // Can never access self.tail after this!
        let Node {
            value,
            prev: penultimate,
            ..
        } = unsafe { *Box::from_raw(tail.as_ptr()) };

        if let Some(penultimate) = penultimate {
            unsafe { (*penultimate.as_ptr()).next = None };
            self.tail = Some(penultimate);
        } else {
            self.head = None;
            self.tail = None;
        }

        Some(value)
    }

    /// SAFETY: The node pointer must point to a node in the given `LinkedList`
    pub unsafe fn remove_node(&mut self, ptr: *mut Node<T>) -> T {
        // SAFETY: Cannot access ptr after this
        let boxed = unsafe { Box::from_raw(ptr) };
        let Node { next, prev, value } = *boxed;

        if let Some(prev) = prev {
            (*prev.as_ptr()).next = next;
        } else {
            self.head = next;
        }

        if let Some(next) = next {
            (*next.as_ptr()).prev = prev;
        } else {
            self.tail = prev;
        }
        value
    }

    pub fn head_node(&self) -> Option<NonNull<Node<T>>> {
        self.head
    }

    pub fn tail_node(&self) -> Option<NonNull<Node<T>>> {
        self.tail
    }

    pub fn is_empty(&self) -> bool {
        debug_assert_eq!(self.head.is_none(), self.tail.is_none());
        self.head.is_none()
    }

    pub fn iter(&self) -> Iter<T> {
        Iter {
            head: self.head.map(|x| x.as_ptr()).unwrap_or(null_mut()),
            tail: self.tail.map(|x| x.as_ptr()).unwrap_or(null_mut()),
            marker: PhantomData,
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<T> {
        IterMut {
            head: self.head.map(|x| x.as_ptr()).unwrap_or(null_mut()),
            tail: self.tail.map(|x| x.as_ptr()).unwrap_or(null_mut()),
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
                self.head = match (*node).next {
                    Some(node) => node.as_ptr(),
                    None => std::ptr::null_mut(),
                };
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
                self.tail = match (*node).prev {
                    Some(node) => node.as_ptr(),
                    None => std::ptr::null_mut(),
                };
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
                self.head = match (*node).next {
                    Some(node) => node.as_ptr(),
                    None => std::ptr::null_mut(),
                };
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
                self.tail = match (*node).prev {
                    Some(node) => node.as_ptr(),
                    None => std::ptr::null_mut(),
                };
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

        #[test]
    fn iterator() {
        let vec_1 = vec![1, 2, 3, 4, 5];
        let list = LinkedList::from(vec_1.clone());
        let vec_2 = list.collect::<Vec<_>>();
        assert_eq!(vec_1, vec_2);
    }


    #[test]
    fn iter() {
        struct NonCopyInt(i32);
        let vec: Vec<NonCopyInt> = [1, 2, 3, 4, 5].into_iter().map(NonCopyInt).collect();
        let list = LinkedList::from(vec);
        let _: Vec<&NonCopyInt> = list.iter().collect();
    }

    #[test]
    fn iter_mut() {
        let vec = vec![1, 2, 3, 4, 5];
        let mut list = LinkedList::from(vec);
        for element in list.iter_mut() {
            *element += 1;
        }
        let vec = list.collect::<Vec<_>>();
        assert_eq!(vec, [2, 3, 4, 5, 6]);
    }

    #[test]
    fn remove_node() {
        let mut list = LinkedList::from([1, 2, 3]);
        let first = list.head_node().unwrap();
        let middle = list.tail_node().unwrap();
        list.push_back(4);
        list.push_back(5);
        let last = list.tail_node().unwrap();

        unsafe {
            list.remove_node(first.as_ptr());
            list.remove_node(middle.as_ptr());
            list.remove_node(last.as_ptr());
        }
    }
}
