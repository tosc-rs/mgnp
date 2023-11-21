use super::*;
use crate::loom::sync::Arc;
use alloc::boxed::Box;

use super::channel_core::{Core, CoreVtable};

/// TrickyPipe is an allocated Tricky Pipe.
///
/// This variant is intended for use on systems with a heap allocator.
//
// TODO(eliza): we should probably replace the use of `Arc` here with manual ref
// counting, since the `Core` tracks the number of senders and receivers
// already. But, I was in a hurry to get a prototype working...
pub struct TrickyPipe<T, E = ()>(Arc<Inner<T, E>>)
where
    T: 'static,
    E: Clone + 'static;

struct Inner<T, E>
where
    T: 'static,
    E: Clone + 'static,
{
    core: Core<E>,
    // TODO(eliza): instead of boxing the elements array, we should probably
    // manually allocate a `Layout`. This works for now, though.
    //
    // TODO(eliza): Also, when we do that, we'll want to make it possible to
    // integrate with `mnemos-alloc`. I think we can do that by adding functions
    // like this:
    //  - `pub const fn layout_for(capacity: u8) -> Layout`
    //  - `pub unsafe fn from_raw(ptr: *const (), layout: Layout) -> Self`
    elements: Box<[Cell<T>]>,
}

impl<T: 'static, E: Clone + 'static> TrickyPipe<T, E> {
    /// Create a new [`TrickyPipe`] allocated on the heap.
    ///
    /// NOTE: `CAPACITY` MUST be a power of two, and must also be <= the number of bits
    /// in a `usize`, e.g. <= 64 on a 64-bit system.
    // TODO(eliza): we would need to add a mnemos-alloc version of this...
    pub fn new(capacity: u8) -> Self {
        Self(Arc::new(Inner {
            core: Core::new(capacity),
            elements: (0..capacity)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
        }))
    }

    const CORE_VTABLE: &'static CoreVtable<E> = &CoreVtable {
        get_core: Self::get_core,
        get_elems: Self::get_elems,
        clone: Self::erased_clone,
        drop: Self::erased_drop,
        type_name: core::any::type_name::<T>,
    };

    fn erased(&self) -> ErasedPipe<E> {
        let ptr = Arc::into_raw(self.0.clone()) as *const _;
        unsafe { ErasedPipe::new(ptr, Self::CORE_VTABLE) }
    }

    fn typed(&self) -> TypedPipe<T, E> {
        unsafe { self.erased().typed() }
    }
    /// Try to obtain a [`Receiver<T>`] capable of receiving `T`-typed data
    ///
    /// This method will only return [`Some`] on the first call. All subsequent calls
    /// will return [`None`].
    pub fn receiver(&self) -> Option<Receiver<T, E>> {
        self.0.core.try_claim_rx()?;

        Some(Receiver {
            pipe: self.typed(),
            closed_error: false,
        })
    }

    /// Obtain a [`Sender<T>`] capable of sending `T`-typed data
    ///
    /// This function may be called multiple times.
    pub fn sender(&self) -> Sender<T, E> {
        self.0.core.add_tx();
        Sender { pipe: self.typed() }
    }

    unsafe fn get_core(ptr: *const ()) -> *const Core<E> {
        unsafe {
            let ptr = ptr.cast::<Inner<T, E>>();
            ptr::addr_of!((*ptr).core)
        }
    }

    unsafe fn get_elems(ptr: *const ()) -> ErasedSlice {
        let ptr = ptr.cast::<Inner<T, E>>();
        ErasedSlice::erase(&(*ptr).elements)
    }

    unsafe fn erased_clone(ptr: *const ()) {
        test_println!("erased_clone({ptr:p})");
        Arc::increment_strong_count(ptr.cast::<Inner<T, E>>())
    }

    unsafe fn erased_drop(ptr: *const ()) {
        let arc = Arc::from_raw(ptr.cast::<Inner<T, E>>());
        test_println!(refs = Arc::strong_count(&arc), "erased_drop({ptr:p})");
        drop(arc)
    }
}

impl<T, E> TrickyPipe<T, E>
where
    T: Serialize + Send + 'static,
    E: Clone + Send + Sync,
{
    /// Try to obtain a [`SerReceiver`] capable of receiving bytes containing
    /// a serialized instance of `T`.
    ///
    /// This method will only return [`Some`] on the first call. All subsequent calls
    /// will return [`None`].
    pub fn ser_receiver(&self) -> Option<SerReceiver<E>> {
        self.0.core.try_claim_rx()?;

        Some(SerReceiver {
            pipe: self.erased(),
            vtable: Self::SER_VTABLE,
            closed_error: false,
        })
    }

    const SER_VTABLE: &'static SerVtable = &SerVtable {
        #[cfg(any(test, feature = "alloc"))]
        to_vec: SerVtable::to_vec::<T>,
        #[cfg(any(test, feature = "alloc"))]
        to_vec_framed: SerVtable::to_vec_framed::<T>,
        to_slice: SerVtable::to_slice::<T>,
        to_slice_framed: SerVtable::to_slice_framed::<T>,
        drop_elem: SerVtable::drop_elem::<T>,
    };
}

impl<T, E> TrickyPipe<T, E>
where
    T: DeserializeOwned + Send + 'static,
    E: Clone + Send + Sync,
{
    /// Try to obtain a [`DeserSender`] capable of sending bytes containing
    /// a serialized instance of `T`.
    ///
    /// This method will only return [`Some`] on the first call. All subsequent calls
    /// will return [`None`].
    pub fn deser_sender(&self) -> DeserSender<E> {
        self.0.core.add_tx();
        DeserSender {
            pipe: self.erased(),
            vtable: Self::DESER_VTABLE,
        }
    }

    const DESER_VTABLE: &'static DeserVtable = &DeserVtable::new::<T>();
}

impl<T, E: Clone> Clone for TrickyPipe<T, E> {
    fn clone(&self) -> Self {
        test_span!("TrickyPipe::clone");
        // Since the `TrickyPipe` type can construct new `Sender`s, this
        // "counts" as cloning a sender with regards to the channel's sender
        // count --- the channel cannot close until all `Sender`s *and* all
        // `TrickyPipe`s are dropped.
        self.0.core.add_tx();
        Self(self.0.clone())
    }
}

impl<T, E: Clone> Drop for TrickyPipe<T, E> {
    fn drop(&mut self) {
        test_span!("TrickyPipe::drop");
        // Since the `TrickyPipe` type can construct new `Sender`s, this
        // "counts" as dropping a sender with regards to the channel's sender
        // count --- the channel cannot close until all `Sender`s *and* all
        // `TrickyPipe`s are dropped.
        self.0.core.drop_tx();
    }
}

unsafe impl<T: Send, E: Clone + Send + Sync> Send for TrickyPipe<T, E> {}
unsafe impl<T: Send, E: Clone + Send + Sync> Sync for TrickyPipe<T, E> {}

// === impl Inner ===

impl<T, E: Clone> Drop for Inner<T, E> {
    fn drop(&mut self) {
        test_span!("Inner::drop");

        // TODO(eliza): there is probably a more efficient way to implement this
        // rather than by using `try_dequeue`, since we know that we have
        // exclusive ownership over the queue. But this works.
        while let Ok(res) = self.core.try_dequeue() {
            let idx = res.idx as usize;
            self.elements[test_dbg!(idx)].with_mut(|ptr| unsafe {
                (*ptr).as_mut_ptr().drop_in_place();
            });
        }
    }
}
