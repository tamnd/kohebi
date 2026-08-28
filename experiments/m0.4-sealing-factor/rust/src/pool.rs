//! A size-classed pooling allocator, so `open` can be measured against CPython
//! on equal allocator terms.
//!
//! The first run of this experiment had `open` at 2.1x *slower* than CPython,
//! which would have been a serious result about the float decision in
//! `03-object-model.md`. It is only a result if the two runtimes are allocating
//! the same way, and they were not. CPython has run its own small-object
//! allocator since 2.3, plus a dedicated free list for floats, so a Python float
//! add recycles a block off a list. `Rc::new` goes to the system allocator every
//! time. Comparing those measures the allocators, not the object models.
//!
//! So this is the second bracket. Build with `--features pool` and small
//! allocations come off a per-size-class free list carved out of megabyte chunks,
//! which is what `obmalloc` does. The difference between the two builds is the
//! part of `open`'s cost that is the allocator's fault and can be engineered
//! away, and what is left is the part that belongs to boxing floats at all.
//!
//! Only ever exercised single-threaded, but a `GlobalAlloc` has to be `Sync`, so
//! the free lists sit behind a spin lock. It is uncontended in the benchmark and
//! costs one uncontended atomic swap per allocation, which a real runtime would
//! also pay unless its pools were thread-local.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

/// Blocks up to this size come from the pool. An `Rc<Obj>` holding a float is
/// two reference counts plus the `Obj` enum, and the enum is as wide as its
/// largest variant, so it lands near a hundred bytes rather than the twenty-four
/// CPython uses. Sizing the classes to cover that is deliberate: the point is to
/// price the allocator, and a pool that spilled every float to the system
/// allocator would price nothing.
const MAX_POOLED: usize = 128;
const GRANULE: usize = 16;
const CLASSES: usize = MAX_POOLED / GRANULE;
const CHUNK: usize = 1 << 20;

struct Lists {
    free: [*mut u8; CLASSES],
    /// The bump cursor into the current chunk, and what is left of it.
    cursor: *mut u8,
    left: usize,
}

static LOCK: AtomicBool = AtomicBool::new(false);
static mut LISTS: Lists = Lists {
    free: [ptr::null_mut(); CLASSES],
    cursor: ptr::null_mut(),
    left: 0,
};

#[inline(always)]
fn class_of(layout: Layout) -> Option<usize> {
    if layout.size() == 0 || layout.size() > MAX_POOLED || layout.align() > GRANULE {
        return None;
    }
    Some((layout.size() - 1) / GRANULE)
}

#[inline(always)]
fn lock() {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::hint::spin_loop();
    }
}

#[inline(always)]
fn unlock() {
    LOCK.store(false, Ordering::Release);
}

pub struct Pool;

unsafe impl GlobalAlloc for Pool {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(class) = class_of(layout) else {
            return unsafe { System.alloc(layout) };
        };
        let size = (class + 1) * GRANULE;
        lock();
        // SAFETY: the lock is held, so this is the only live reference.
        let lists = unsafe { &mut *ptr::addr_of_mut!(LISTS) };
        let head = lists.free[class];
        let block = if !head.is_null() {
            // The next pointer lives in the block itself, which is why the
            // smallest class has to be at least a pointer wide.
            lists.free[class] = unsafe { *(head as *mut *mut u8) };
            head
        } else {
            if lists.left < size {
                let chunk = unsafe { System.alloc(Layout::from_size_align_unchecked(CHUNK, GRANULE)) };
                if chunk.is_null() {
                    unlock();
                    return ptr::null_mut();
                }
                lists.cursor = chunk;
                lists.left = CHUNK;
            }
            let block = lists.cursor;
            lists.cursor = unsafe { lists.cursor.add(size) };
            lists.left -= size;
            block
        };
        unlock();
        block
    }

    unsafe fn dealloc(&self, ptr_: *mut u8, layout: Layout) {
        let Some(class) = class_of(layout) else {
            return unsafe { System.dealloc(ptr_, layout) };
        };
        lock();
        let lists = unsafe { &mut *ptr::addr_of_mut!(LISTS) };
        unsafe { *(ptr_ as *mut *mut u8) = lists.free[class] };
        lists.free[class] = ptr_;
        unlock();
    }
}
