//! A type-erased, serializable, reusable shared box thingy.
#![warn(missing_debug_implementations)]
#![allow(missing_docs)] // i'll fix this later...

use crate::{
    loom::{
        cell::UnsafeCell,
        sync::atomic::{AtomicU8, Ordering::*},
    },
    typeinfo::TypeInfo,
};
use core::{fmt, mem::MaybeUninit, ptr::NonNull};
use maitake_sync::WaitCell;

#[cfg(any(test, feature = "alloc"))]
use crate::loom::alloc::boxed::Box;

#[cfg(any(test, feature = "alloc"))]
use alloc::vec::Vec;

pub struct Sharer<T> {
    shared: NonNull<SerBox<T>>,
    drop_shared: unsafe fn(NonNull<SerBox<()>>),
}

#[must_use = "a Consumer does nothing if `to_vec`, `to_vec_framed`, \
             `to_slice`, or `to_slice_framed` are not called"]
pub struct Consumer {
    shared: NonNull<SerBox<()>>,
    vtable: &'static Vtable,
    drop_shared: unsafe fn(NonNull<SerBox<()>>),
}

#[repr(C)]
pub struct SerBox<T> {
    state: AtomicU8,
    typeinfo: TypeInfo,
    sharer_ready: WaitCell,
    data: UnsafeCell<MaybeUninit<T>>,
}

type SliceFn = fn(NonNull<SerBox<()>>, &mut [u8]) -> postcard::Result<&mut [u8]>;

struct Vtable {
    #[cfg(any(test, feature = "alloc"))]
    to_vec: fn(NonNull<SerBox<()>>) -> postcard::Result<Vec<u8>>,
    #[cfg(any(test, feature = "alloc"))]
    to_vec_framed: fn(NonNull<SerBox<()>>) -> postcard::Result<Vec<u8>>,
    to_slice: SliceFn,
    to_slice_framed: SliceFn,
    drop_data: fn(NonNull<SerBox<()>>),
}

const IDLE: u8 = 0;
const HAS_SHARER: u8 = 1 << 0;
const HAS_DATA: u8 = 1 << 1;
const HAS_CONSUMER: u8 = 1 << 2;
const SHARED: u8 = HAS_CONSUMER | HAS_DATA | HAS_SHARER;

impl<T> Sharer<T>
where
    T: serde::Serialize + Send + Sync + 'static,
{
    pub fn try_share(&mut self, value: T) -> Result<Consumer, T> {
        unsafe { self.shared.as_ref() }.try_share(value)?;

        Ok(Consumer {
            shared: self.shared.cast(),
            vtable: SerBox::<T>::VTABLE,
            drop_shared: self.drop_shared,
        })
    }

    pub async fn share(&mut self, mut value: T) -> Consumer {
        loop {
            match self.try_share(value) {
                Ok(consumer) => return consumer,
                Err(v) => {
                    value = v;
                    unsafe { self.shared.as_ref() }
                        .sharer_ready
                        .wait()
                        .await
                        .expect("sharer_ready waitcell is never closed");
                }
            }
        }
    }
}

impl<T> Drop for Sharer<T> {
    fn drop(&mut self) {
        let state = unsafe { self.shared.as_ref() }
            .state
            .fetch_and(!(HAS_SHARER | HAS_DATA), AcqRel);
        if state & HAS_DATA != 0 {
            unsafe { self.shared.as_ref() }
                .data
                .with_mut(|data| unsafe { core::ptr::drop_in_place((*data).as_mut_ptr()) })
        }

        if state & HAS_CONSUMER == 0 {
            // no consumer, we can deallocate
            unsafe {
                (self.drop_shared)(self.shared.cast());
            }
        }
    }
}

impl<T> fmt::Debug for Sharer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            shared,
            drop_shared,
        } = self;
        f.debug_struct("Sharer")
            .field("shared", &unsafe { shared.as_ref() })
            .field("drop_shared", &drop_shared)
            .finish()
    }
}

unsafe impl<T: Send + Sync> Send for Sharer<T> {}
unsafe impl<T: Send + Sync> Sync for Sharer<T> {}

// === impl Consumer ===

impl Consumer {
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec(&self) -> postcard::Result<Vec<u8>> {
        (self.vtable.to_vec)(self.shared)
    }

    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec_framed(&self) -> postcard::Result<Vec<u8>> {
        (self.vtable.to_vec_framed)(self.shared)
    }

    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.shared, buf)
    }

    pub fn to_slice_framed<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice_framed)(self.shared, buf)
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        unsafe { self.shared.as_ref() }
            .state
            .fetch_and(!HAS_CONSUMER, AcqRel);
        (self.vtable.drop_data)(self.shared);

        let state = unsafe { self.shared.as_ref() }
            .state
            .fetch_and(!HAS_DATA, AcqRel);
        if state & HAS_SHARER == 0 {
            // no sharer, we can deallocate
            unsafe { (self.drop_shared)(self.shared) }
        } else {
            // sharer exists, let them know that we're ready!
            unsafe {
                self.shared.as_ref().sharer_ready.wake();
            }
        }
    }
}

impl fmt::Debug for Consumer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            shared,
            drop_shared,
            vtable,
        } = self;
        f.debug_struct("Consumer")
            .field("shared", unsafe { shared.as_ref() })
            .field("drop_shared", &drop_shared)
            .field("vtable", &format_args!("{vtable:p}"))
            .finish()
    }
}

// Safety: `Consumer`s can only be constructed from `SerBox`es whose `T` is
// `Send + Sync`...
unsafe impl Send for Consumer {}

// Safety: `Consumer`s can only be constructed from `SerBox`es whose `T` is
// `Send + Sync`...
unsafe impl Sync for Consumer {}

// === impl SerBox ===

impl<T> SerBox<T>
where
    T: serde::Serialize + Send + Sync + 'static,
{
    #[cfg(any(test, feature = "alloc"))]
    pub fn sharer(self: Box<Self>) -> Sharer<T> {
        self.state
            .compare_exchange(IDLE, HAS_SHARER, AcqRel, Acquire)
            .expect("this SerBox was already converted into a Sharer");
        Sharer {
            shared: unsafe { NonNull::new_unchecked(Box::into_raw(self)) },
            drop_shared: Self::drop_box,
        }
    }

    pub fn static_sharer(&'static self) -> Option<Sharer<T>> {
        self.state
            .compare_exchange(IDLE, HAS_SHARER, AcqRel, Acquire)
            .ok()?;

        Some(Sharer {
            shared: NonNull::from(self),
            drop_shared: Self::drop_static,
        })
    }

    #[cfg(not(loom))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            typeinfo: TypeInfo::of::<T>(),
            sharer_ready: WaitCell::new(),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[cfg(loom)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            typeinfo: TypeInfo::of::<T>(),
            sharer_ready: WaitCell::new(),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    const VTABLE: &'static Vtable = &Vtable {
        #[cfg(any(test, feature = "alloc"))]
        to_vec: |ptr| Self::with_data(ptr, postcard::to_allocvec),
        #[cfg(any(test, feature = "alloc"))]
        to_vec_framed: |ptr| Self::with_data(ptr, postcard::to_allocvec_cobs),
        to_slice: |ptr, buf| Self::with_data(ptr, move |data| postcard::to_slice(data, buf)),
        to_slice_framed: |ptr, buf| {
            Self::with_data(ptr, move |data| postcard::to_slice_cobs(data, buf))
        },
        drop_data: |ptr| unsafe {
            Self::from_erased(ptr)
                .as_ref()
                .data
                .with_mut(|data| core::ptr::drop_in_place((*data).as_mut_ptr()))
        },
    };

    #[inline]
    #[must_use]
    fn from_erased(ptr: NonNull<SerBox<()>>) -> NonNull<Self> {
        unsafe {
            ptr.as_ref().typeinfo.assert_matches::<T>("SerBox");
        }
        ptr.cast::<SerBox<T>>()
    }

    #[inline]
    #[must_use]
    fn with_data<U>(ptr: NonNull<SerBox<()>>, f: impl FnOnce(&T) -> U) -> U {
        unsafe {
            Self::from_erased(ptr).as_ref().data.with(|data| {
                // Safety: if a `Consumer` has been constructed, then the data has
                // been initialized.
                let data = (*data).assume_init_ref();
                f(data)
            })
        }
    }

    fn try_share(&self, value: T) -> Result<(), T> {
        if self
            .state
            .compare_exchange(HAS_SHARER, SHARED, AcqRel, Acquire)
            .is_err()
        {
            return Err(value);
        }

        self.data.with_mut(|data| unsafe {
            (*data).write(value);
        });

        Ok(())
    }

    #[cfg(any(test, feature = "alloc"))]
    unsafe fn drop_box(ptr: NonNull<SerBox<()>>) {
        let ptr = ptr.cast::<Self>().as_ptr();
        drop(Box::from_raw(ptr))
    }

    unsafe fn drop_static(_: NonNull<SerBox<()>>) {
        // nop
    }
}

impl<T> fmt::Debug for SerBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            state,
            typeinfo,
            sharer_ready,
            data: _,
        } = self;
        f.debug_struct("SerBox")
            .field("state", &format_args!("{:#02b}", state.load(Acquire)))
            .field("sharer_ready", &sharer_ready)
            .field(
                "data",
                &format_args!("UnsafeCell<MaybeUninit<{}>>", typeinfo.name()),
            )
            .finish()
    }
}

unsafe impl<T: Send + Sync> Send for SerBox<T> {}
unsafe impl<T: Send + Sync> Sync for SerBox<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loom;

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    pub(crate) struct SerStruct {
        pub(crate) a: u8,
        pub(crate) b: i16,
        pub(crate) c: u32,
        pub(crate) d: String,
    }

    fn basically_works(mut sharer: Sharer<SerStruct>) {
        let val1 = SerStruct {
            a: 1,
            b: 2,
            c: 3,
            d: "hello".into(),
        };
        let ser1 = postcard::to_allocvec(&val1);
        let val2 = SerStruct {
            a: 4,
            b: 5,
            c: 6,
            d: "world".into(),
        };
        let ser2 = postcard::to_allocvec(&val2);

        let cons = test_dbg!(sharer.try_share(val1))
            .expect("try_share should succeed while no consumer exists");

        let val2 = test_dbg!(sharer.try_share(val2))
            .expect_err("a consumer exists, so try_share should fail");

        assert_eq!(test_dbg!(cons.to_vec()), ser1);

        test_dbg!(drop(cons));

        let cons = test_dbg!(sharer.try_share(val2))
            .expect("try_share should succeed now that the first consumer has been dropped");

        assert_eq!(test_dbg!(cons.to_vec()), ser2);
    }

    #[test]
    fn box_basically_works() {
        loom::model(|| {
            let serbox = Box::new(SerBox::<SerStruct>::new());
            basically_works(serbox.sharer());
        })
    }

    #[test]
    #[cfg(not(loom))] // static doesn't work under loom
    fn static_basically_works() {
        static SERBOX: SerBox<SerStruct> = SerBox::new();
        loom::model(|| {
            basically_works(
                SERBOX
                    .static_sharer()
                    .expect("no static sharer has been claimed"),
            );
        })
    }

    #[test]
    fn cross_thread_sharing() {
        loom::model(|| {
            let mut sharer = Box::new(SerBox::<SerStruct>::new()).sharer();
            let val1 = SerStruct {
                a: 1,
                b: 2,
                c: 3,
                d: "hello".into(),
            };
            let ser1 = postcard::to_allocvec(&val1);
            let val2 = SerStruct {
                a: 4,
                b: 5,
                c: 6,
                d: "world".into(),
            };
            let ser2 = postcard::to_allocvec(&val2);

            let cons = loom::future::block_on(sharer.share(val1));
            let t1 = loom::thread::spawn(move || {
                assert_eq!(test_dbg!(cons.to_vec()), ser1);
                test_dbg!(drop(cons));
            });

            let cons = loom::future::block_on(sharer.share(val2));
            let t2 = loom::thread::spawn(move || {
                assert_eq!(test_dbg!(cons.to_vec()), ser2);
                test_dbg!(drop(cons));
            });

            t1.join().expect("thread1 panicked");
            t2.join().expect("thread2 panicked");
        })
    }
}
