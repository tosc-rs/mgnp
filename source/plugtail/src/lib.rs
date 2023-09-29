//! Plugtail provides an abstraction over:
//!
//! * A `Sized` Header of a given type
//! * An unsized array of a given type
//!
//! This aims to abstract over writing code that *sometimes* needs static storage,
//! like a pre-allocated channel on an embedded system, while in other cases you
//! want to allocate the storage in an Arc-shaped container at runtime.
//!
//! Additionally, the unsized array is all UnsafeCell and MaybeUninit, because
//! we assume you might want to do horrific cursed inner mutibilitiy things to it,
//! as is customary for queues and such.
//!
//! For this reason, the header should also know how to "tear down" the unsized storage
//! if necessary.

#![cfg_attr(not(test), no_std)]

use core::{cell::UnsafeCell, mem::MaybeUninit};

/// This is the main trait that data structure writers should use.
///
/// They should probably always use a fixed Header type, and be generic over a given T.
pub trait Pluggable: Clone {
    type Header: BodyDrop<Item = Self::Item>;
    type Item;
    fn storage(&self) -> PlugDat<'_, Self::Header, Self::Item>;
    fn leak(self) -> *const ();
    unsafe fn unleak(p: *const ()) -> Self;
}

/// This trait describes how a given Header type can tear down the body/tail when the total
/// `Pluggable` item is dropped.
pub trait BodyDrop {
    type Item;
    fn body_drop(&self, i: &[UnsafeCell<MaybeUninit<Self::Item>>]);
}

/// This is what you get back from `Pluggable::storage()`.
///
/// Your data structures should generally use this to access the header and storage
/// tail data.
pub struct PlugDat<'a, H, T> {
    pub hdr: &'a H,
    pub t: &'a [UnsafeCell<MaybeUninit<T>>],
}

/// Helper struct to ensure alignment and used with `addr_of!()` to find the start
/// of the unsized tail region
#[repr(C)]
pub struct PlugTail<H, T> {
    hdr: H,
    tail: [T; 0],
}

#[cfg(feature = "use-alloc")]
pub mod alloc {
    extern crate alloc;

    use core::{
        ptr::{addr_of, addr_of_mut, drop_in_place, NonNull},
        sync::atomic::{AtomicUsize, Ordering},
    };
    use alloc::alloc::{dealloc, Layout};
    use super::*;

    /// An Arc-like heap allocated Pluggable
    pub struct ArcPlugTail<H, T>
    where
        H: BodyDrop<Item = T>,
    {
        pt: NonNull<PlugTail<ArcHdr<H>, T>>,
    }

    unsafe impl<H: Send + BodyDrop<Item = T>, T: Send> Send for ArcPlugTail<H, T> { }
    unsafe impl<H: Send + BodyDrop<Item = T>, T: Send> Sync for ArcPlugTail<H, T> { }

    #[repr(C)]
    pub struct ArcHdr<H> {
        strong: AtomicUsize,
        len: usize,
        hdr: H,
    }

    impl<H, T> Clone for ArcPlugTail<H, T>
    where
        H: BodyDrop<Item = T>,
    {
        fn clone(&self) -> Self {
            unsafe {
                self.pt.as_ref().hdr.strong.fetch_add(1, Ordering::AcqRel);
            }
            Self { pt: self.pt }
        }
    }

    impl<H, T> Drop for ArcPlugTail<H, T>
    where
        H: BodyDrop<Item = T>,
    {
        fn drop(&mut self) {
            {
                let ptref = unsafe { self.pt.as_ref() };
                if ptref.hdr.strong.fetch_sub(1, Ordering::AcqRel) > 1 {
                    return;
                }
            }
            // drop slow or whatever
            let n = {
                let s = self.storage();
                s.hdr.body_drop(s.t);
                s.t.len()
            };
            unsafe {
                drop_in_place(self.pt.as_ptr());
                dealloc(self.pt.as_ptr().cast(), Self::layout(n));
            }
        }
    }

    impl<H, T> ArcPlugTail<H, T>
    where
        H: BodyDrop<Item = T>,
    {
        pub fn layout(n: usize) -> Layout {
            let layout = Layout::new::<PlugTail<ArcHdr<H>, T>>();
            let (layout, _) = layout.extend(Layout::array::<T>(n).unwrap()).unwrap();
            layout
        }

        pub fn new(hdr: H, n: usize) -> Self {
            let layout = Self::layout(n);
            let ptr = NonNull::new(
                unsafe { alloc::alloc::alloc(layout) }.cast::<PlugTail<ArcHdr<H>, T>>(),
            )
            .unwrap();
            unsafe {
                let hptr = addr_of_mut!((*ptr.as_ptr()).hdr);
                hptr.write(ArcHdr {
                    strong: AtomicUsize::new(1),
                    len: n,
                    hdr,
                });
            }

            ArcPlugTail { pt: ptr }
        }
    }

    impl<H, T> Pluggable for ArcPlugTail<H, T>
    where
        H: BodyDrop<Item = T>,
    {
        type Header = H;
        type Item = T;

        fn storage(&self) -> PlugDat<'_, Self::Header, Self::Item> {
            let base_ptr = self.pt.as_ptr();

            unsafe {
                let len = (*base_ptr).hdr.len;
                let hdr_ptr: *const H = addr_of!((*base_ptr).hdr.hdr).cast();
                let bod_ptr: *const UnsafeCell<MaybeUninit<T>> = addr_of!((*base_ptr).tail).cast();
                let bod_sli = core::slice::from_raw_parts(bod_ptr, len);
                PlugDat {
                    hdr: &*hdr_ptr,
                    t: bod_sli,
                }
            }
        }

        fn leak(self) -> *const () {
            let base_ptr = self.pt.as_ptr();
            core::mem::forget(self);
            base_ptr.cast()
        }

        unsafe fn unleak(ptr: *const ()) -> Self {
            // welp
            Self { pt: NonNull::new_unchecked(ptr.cast_mut().cast()) }
        }
    }
}

/// A statically allocated PlugTail.
///
/// This is what you want for embedded.
#[repr(C)]
pub struct StaticPlugTail<H, T, const N: usize> {
    pt: PlugTail<H, T>,
    tfr: [UnsafeCell<MaybeUninit<T>>; N],
}

// TODO: should StaticPlugTail have any kind of blanket impl for
// Send or Sync?
unsafe impl<H: Send, T: Send, const N: usize> Send for StaticPlugTail<H, T, N> { }
unsafe impl<H: Send, T: Send, const N: usize> Sync for StaticPlugTail<H, T, N> { }

impl<H, T, const N: usize> StaticPlugTail<H, T, N> {
    const ONE: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());

    pub const fn uninit(hdr: H) -> Self {
        Self {
            pt: PlugTail { hdr, tail: [] },
            tfr: [Self::ONE; N],
        }
    }

    // TODO: Is there any way we could make this not UC<MU<T>> and instead pass
    // a [T; N]? That would be a much nicer API...
    pub const fn new(hdr: H, val: [UnsafeCell<MaybeUninit<T>>; N]) -> Self
    {
        Self {
            pt: PlugTail { hdr, tail: [] },
            tfr: val,
        }
    }
}

// Note that Pluggable is implemented for a REFERENCE to a static, NOT on a static
// itself! This is intentional: this allows us to keep the "arc-like" cloneability
// without making a copy to the underlying storage.
//
// However, this means in practice most people will need to have separate "storage"
// and "do stuff" structures. This could mean something like:
//
// ```rust,skip
// // This is where the `StaticPlugTail` goes
// static WSTO: WhateverStorage = WhateverStorage::new();
//
// // This is the actual data structure
// static WHATEVER: Whatever = Whatever::new(&WSTO);
// ```
//
// OR
//
// ```rust,skip
// // This is where the `StaticPlugTail` goes
// static WSTO: WhateverStorage = WhateverStorage::new();
//
// fn main() {
//     let whatever: Whatever = WSTO.take().unwrap();
// }
// ```

impl<H, T, const N: usize> Pluggable for &'static StaticPlugTail<H, T, N>
where
    H: BodyDrop<Item = T>,
{
    type Header = H;
    type Item = T;

    fn storage(&self) -> PlugDat<'_, Self::Header, Self::Item> {
        PlugDat {
            hdr: &self.pt.hdr,
            t: &self.tfr,
        }
    }

    fn leak(self) -> *const () {
        let ptr: &'static StaticPlugTail<H, T, N> = self;
        let ptr: *const StaticPlugTail<H, T, N> = ptr;
        let ptr: *const () = ptr.cast();
        ptr
    }

    unsafe fn unleak(p: *const ()) -> Self {
        let ptr: *const StaticPlugTail<H, T, N> = p.cast();
        &*ptr
    }
}
