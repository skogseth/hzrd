use std::ptr::NonNull;

/// Place object on the heap (will leak)
pub fn allocate<T>(object: T) -> NonNull<T> {
    let raw = Box::into_raw(Box::new(object));
    // SAFETY: The boxed ptr itself is never null
    unsafe { NonNull::new_unchecked(raw) }
}

/// Free heap allocated memory
/// SAFETY: Must point to valid heap-allocated memory
pub unsafe fn free<T>(non_null_ptr: NonNull<T>) {
    let _ = Box::from_raw(non_null_ptr.as_ptr());
}
