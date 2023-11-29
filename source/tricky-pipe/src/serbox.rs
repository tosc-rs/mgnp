//! A type-erased, serializable, reusable shared box thingy.
#![warn(missing_debug_implementations)]
use crate::{
    loom::{
        cell::{CellWith, UnsafeCell},
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

/// Shares a `T`-typed value as a type-erased, serializable [`Consumer`].
///
/// A `Sharer` is an owning reference to a shareable memory location (either a
/// heap-allocated [`Box`] or a `static`) which contains a [`SerBox`]`<T>`. The
/// `Sharer` can be used to store a `T`-typed value in that memory location,
/// producing a [`Consumer`], a type-erased reference to that value which can be
/// used to serialize the value.
///
/// A `Sharer` may only store a single value at a time, constructing a single
/// [`Consumer`]. The value remains shared as long as the [`Consumer`] exists.
/// When the [`Consumer`] is dropped, the shared value is also dropped, and the
/// `Sharer` may be used to share a new value.
///
/// Values can be shared using the [`Sharer::try_share`] method, which returns
/// an error if a previously-shared value is still being used, or
/// [`Sharer::share`], which will asynchronously wait until any currently alive
/// [`Consumer`]s are dropped before sharing a new value.
///
/// A `Sharer` can be constructed from a [`Box`]`<`[`SerBox`]`<T>>` using
/// [`SerBox::sharer`], if the "alloc" crate feature flag is enabled.
/// Alternatively, one may be constructed from an `&'static `[`SerBox`]`<T>`
/// using [`SerBox::static_sharer`].
///
/// When the "alloc" feature flag is enabled, the [`Sharer::new`] and
/// [`Sharer::default`] methods provide shorthand for constructing a `Sharer`
/// from a [`Box`]`<`[`SerBox`]`<T>>`.
#[must_use = "a Sharer does nothing if `share` or `try_share` are not called"]
pub struct Sharer<T> {
    shared: NonNull<SerBox<T>>,
    drop_shared: unsafe fn(NonNull<SerBox<()>>),
}

/// A type-erased owning reference to a serializable value shared by a
/// [`Sharer`].
///
/// A `Consumer` may be used to serialize the referenced value to a byte slice
/// using the [`Consumer::to_slice`] method. The [`Consumer::to_slice_framed`]
/// method serializes the referenced value to a byte slice as a COBS frame.
///
/// If the "alloc" crate feature flag is enabled, the [`Consumer::to_vec`] and
/// [`Consumer::to_vec_framed`] methods may be used to serialize the value to a
/// [`Vec`]`<u8>`.
///
/// The shared value is (conceptually) owned by the `Consumer`, and dropping the
/// `Consumer` will drop the value. Once the `Consumer` has been dropped, the
/// [`Sharer`] may share another value, producing a new `Consumer`.
#[must_use = "a Consumer does nothing if `to_vec`, `to_vec_framed`, \
             `to_slice`, or `to_slice_framed` are not called"]
pub struct Consumer {
    shared: NonNull<SerBox<()>>,
    vtable: &'static Vtable,
    drop_shared: unsafe fn(NonNull<SerBox<()>>),
}

/// The shared memory location used by [`Sharer`]s and [`Consumer`]s to store a
/// shared value.
///
/// This type is not used directly. Instead, it is used to construct a
/// [`Sharer`] when it has been stored in a shareable memory location. This may
/// be a `static SerBox<T>`, using the [`SerBox::static_sharer`] method, or, if
/// the "alloc" crate feature flag is enabled, a [`Box`]`<`[`SerBox`]`<T>>`,
/// using [`SerBox::sharer`].
///
/// When using "alloc", users need not construct a `SerBox` directly, and can
/// instead construct [`Sharer`]s using [`Sharer::new`] or [`Sharer::default`].
/// However, when using a `static` to store the `SerBox`, it is necessary to
/// construct instances of `SerBox` as part of the static initializer.
///
/// For example:
/// ```
/// use tricky_pipe::serbox::SerBox;
///
/// static MY_SERBOX: SerBox<u32> = SerBox::new();
///
/// let sharer = MY_SERBOX.static_sharer()
///     .expect("no Sharer has been constructed yet, so we can claim it");
///
/// // ... use the `Sharer` to share type-erased values ...
/// # drop(sharer);
/// ```
#[must_use = "a SerBox does nothing if it is not used to construct a `Sharer`"]
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

const NEW: u8 = 0;
const HAS_SHARER: u8 = 1 << 0;
const HAS_DATA: u8 = 1 << 1;
const HAS_CONSUMER: u8 = 1 << 2;
const SHARED: u8 = HAS_CONSUMER | HAS_DATA | HAS_SHARER;

impl<T> Sharer<T>
where
    T: serde::Serialize + Send + Sync + 'static,
{
    /// Returns a new `Sharer`, using a heap-allocated [`Box`] as the shared
    /// memory location.
    ///
    /// This function is only available when the "alloc" crate feature flag is
    /// enabled. When "alloc" is not enabled, a statically-allocated `Sharer`
    /// may be constructed by using [`SerBox::static_sharer`].
    #[cfg(any(test, feature = "alloc"))]
    pub fn new() -> Self {
        Box::new(SerBox::new()).sharer()
    }

    /// Attempts to share `value` as a type-erased serializable [`Consumer`], if
    /// this `Sharer` is not currently sharing a value.
    ///
    /// If a value is currently shared (i.e. a previously-created [`Consumer`]
    /// still exists), this method returns an [`Err`]`(T)` with the shared
    /// value, so that it may be reused by the caller.
    ///
    /// To instead wait asynchronously until a previously-created [`Consumer`]
    /// has been dropped, use [`Sharer::share`] instead.
    ///
    /// # Examples
    ///
    /// Sharing a value:
    ///
    /// ```
    /// use tricky_pipe::serbox::Sharer;
    ///
    /// let mut sharer = Sharer::new();
    ///
    /// // Since no `Consumer` currently exists, `try_share` will succeed.
    /// let consumer = sharer.try_share(42)
    ///     .expect("no value is currently shared");
    ///
    /// // the `Consumer` can be used to serialize the shared value.
    /// let bytes = consumer.to_vec().expect("serialization should succeed");
    /// # drop(bytes);
    /// ```
    ///
    /// A new value can be shared once any previously-existing [`Consumer`] has
    /// been dropped:
    ///
    /// ```
    /// use tricky_pipe::serbox::Sharer;
    ///
    /// let mut sharer = Sharer::new();
    ///
    /// // Share a value:
    /// let consumer = sharer.try_share(42)
    ///     .expect("no value is currently shared");
    ///
    /// // A subsequent call to `try_share` will fail, because a value is
    /// // currently being shared.
    /// let val2 = sharer.try_share(420).expect_err("a value is currently shared");
    ///
    /// // Use the existing `Consumer`, and drop it once we're done.
    /// let bytes = consumer.to_vec().expect("serialization should succeed");
    /// # drop(bytes);
    /// drop(consumer);
    ///
    /// // Now that the previous consumer no longer exists, and the box is
    /// // unoccupied, we can share a new value:
    /// let consumer = sharer.try_share(420)
    ///     .expect("previous Consumer has been dropped");
    /// # drop(consumer);
    /// ```
    pub fn try_share(&mut self, value: T) -> Result<Consumer, T> {
        self.shared().try_share(value)?;

        Ok(Consumer {
            shared: self.shared.cast(),
            vtable: SerBox::<T>::VTABLE,
            drop_shared: self.drop_shared,
        })
    }

    /// Share `value` as a type-erased serializable [`Consumer`].
    ///
    /// If a value is currently shared (i.e. a previously-created [`Consumer`]
    /// still exists), this method will wait until that consumer has been
    /// dropped before sharing the new value. To return an error when a previous
    /// [`Consumer`] already exists, use [`Sharer::try_share`], instead.
    pub async fn share(&mut self, mut value: T) -> Consumer {
        loop {
            match self.try_share(value) {
                Ok(consumer) => return consumer,
                Err(v) => {
                    value = v;
                    test_dbg!(self.shared().sharer_ready.wait().await)
                        .expect("sharer_ready waitcell is never closed");
                }
            }
        }
    }
}

impl<T> Sharer<T> {
    /// Borrows the shared data.
    #[inline]
    fn shared(&self) -> &SerBox<T> {
        unsafe {
            // Safety: the only part of the shared state that is aliased mutably
            // is the `data`, which lives in an `UnsafeCell` anyway. And, it's
            // okay to use `NonNull::as_ref` here, because the returned
            // reference will not outlive this `Sharer`, and the `Sharer` owns
            // the shared memory location pointed to by that `NonNull`...
            self.shared.as_ref()
        }
    }
}

impl<T> Drop for Sharer<T> {
    fn drop(&mut self) {
        let state = test_dbg!(self
            .shared()
            .state
            .fetch_and(!(HAS_SHARER | HAS_DATA), AcqRel));

        // if there's no consumer, we can deallocate...
        if test_dbg!(state) & HAS_CONSUMER == 0 {
            // if there's no consumer, but the `HAS_DATA` bit is still set, this
            // means someone is `mem::forget`ting `Consumer`s. make sure the
            // data gets dropped.
            if test_dbg!(state & HAS_DATA != 0) {
                self.shared().data.with_mut(|data| unsafe {
                    // Safety: since there's no consumer, as indicated by the
                    // `HAS_CONSUMER` bit being unset, we have exclusive mutable
                    // access to the shared memory. And, since the `HAS_DATA`
                    // bit *is* set, we know the data is initialized, so we can
                    // drop it.
                    core::ptr::drop_in_place((*data).as_mut_ptr());
                });
            }

            unsafe {
                // Safety: since there's no consumer, as indicated by the
                // `HAS_CONSUMER` bit being unset, we have exclusive mutable
                // access to the shared memory, and therefore, we can (and
                // must!) deallocate it.
                (self.drop_shared)(self.shared.cast());
            }
        }
    }
}

#[cfg(any(test, feature = "alloc"))]
impl<T> Default for Sharer<T>
where
    T: serde::Serialize + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Sharer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sharer")
            .field("shared", &self.shared())
            .field("drop_shared", &self.drop_shared)
            .finish()
    }
}

unsafe impl<T: Send + Sync> Send for Sharer<T> {}
unsafe impl<T: Send + Sync> Sync for Sharer<T> {}

// === impl Consumer ===

impl Consumer {
    /// Serialize the shared value to a [`postcard`]-encoded [`Vec`]`<u8>`.
    ///
    /// This method is only available if the "alloc" feature flag is enabled.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec(&self) -> postcard::Result<Vec<u8>> {
        (self.vtable.to_vec)(self.shared)
    }

    /// Serialize the shared value to a [`postcard`]-encoded, COBS-framed
    /// [`Vec`]`<u8>`.
    ///
    /// This method is only available if the "alloc" feature flag is enabled.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec_framed(&self) -> postcard::Result<Vec<u8>> {
        (self.vtable.to_vec_framed)(self.shared)
    }

    /// Serialize the shared value to the provided byte slice, using
    /// [`postcard`].
    ///
    /// This method returns the sub-slice of `buf` that contains the serialized
    /// representation of the value.
    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.shared, buf)
    }

    /// Serialize the shared value to the provided byte slice, as a
    /// [`postcard`]-encoded COBS frame.
    ///
    /// This method returns the sub-slice of `buf` that contains the serialized
    /// representation of the value.
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
        if test_dbg!(state & HAS_SHARER) == 0 {
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
    /// Constructs a [`Sharer`]`<T>` from a heap-allocated `SerBox<T>`.
    ///
    /// This method always succeeds, because the `SerBox` is consumed by this
    /// method, preventing a [`Sharer`] from being created a second time.
    #[cfg(any(test, feature = "alloc"))]
    pub fn sharer(self: Box<Self>) -> Sharer<T> {
        self.state
            .compare_exchange(NEW, HAS_SHARER, AcqRel, Acquire)
            .expect("this SerBox was already converted into a Sharer");
        Sharer {
            shared: unsafe { NonNull::new_unchecked(Box::into_raw(self)) },
            drop_shared: Self::drop_box,
        }
    }

    /// Attempts to construct a [`Sharer`]`<T>` from a `&'static SerBox<T>`.
    ///
    /// If the provided `static SerBox<T>` has already been used to construct a
    /// [`Sharer`], this method returns [`None`]. If that [`Sharer`], and any
    /// [`Consumer`]s it has created, are dropped,this method will return
    /// [`Some`] with a new [`Sharer`].
    pub fn static_sharer(&'static self) -> Option<Sharer<T>> {
        self.state
            .compare_exchange(NEW, HAS_SHARER, AcqRel, Acquire)
            .ok()?;

        Some(Sharer {
            shared: NonNull::from(self),
            drop_shared: Self::drop_static,
        })
    }

    /// Returns a new `SerBox<T>`.
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(NEW),
            typeinfo: TypeInfo::of::<T>(),
            sharer_ready: WaitCell::new(),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Returns a new `SerBox<T>`.
    #[cfg(loom)]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(NEW),
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
        if test_dbg!(self
            .state
            .compare_exchange(HAS_SHARER, SHARED, AcqRel, Acquire))
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

    unsafe fn drop_static(ptr: NonNull<SerBox<()>>) {
        let shared = ptr.cast::<Self>().as_ref();
        test_dbg!(shared.state.fetch_and(!HAS_SHARER, Release));
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
            .field("state", &format_args!("{:#06b}", state.load(Acquire)))
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
    fn debug_impls() {
        loom::model(|| {
            let serbox = test_dbg!(Box::new(SerBox::<SerStruct>::new()));
            let mut sharer = test_dbg!(serbox.sharer());
            let _ = test_dbg!(sharer.try_share(SerStruct {
                a: 1,
                b: 2,
                c: 3,
                d: "hello".into()
            }));
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
    #[cfg(not(loom))]
    fn static_is_reusable() {
        static SERBOX: SerBox<SerStruct> = SerBox::new();
        loom::model(|| {
            let mut sharer = test_dbg!(SERBOX.static_sharer()).expect("no sharer exists");

            assert!(
                test_dbg!(SERBOX.static_sharer()).is_none(),
                "new Sharer should not be created while Sharer exists"
            );

            let val1 = SerStruct {
                a: 1,
                b: 2,
                c: 3,
                d: "hello".into(),
            };
            let consumer = test_dbg!(sharer.try_share(val1))
                .expect("try_share should succeed while no Consumer exists");

            test_dbg!(drop(sharer));

            assert!(
                test_dbg!(SERBOX.static_sharer()).is_none(),
                "new Sharer should not be created while Consumer exists"
            );

            test_dbg!(drop(consumer));

            let _sharer = test_dbg!(SERBOX.static_sharer()).expect(
                "new Sharer can be created now that both Sharer and Consumer have been dropped",
            );

            assert!(
                test_dbg!(SERBOX.static_sharer()).is_none(),
                "new Sharer should not be created while Sharer exists"
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
