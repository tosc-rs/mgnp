use super::*;
use crate::loom::sync::Arc;
use alloc::boxed::Box;

use super::channel_core::{Core, CoreVtable};

// TODO(eliza): we should probably replace the use of `Arc` here with manual ref
// counting, since the `Core` tracks the number of senders and receivers
// already. But, I was in a hurry to get a prototype working...
pub struct TrickyPipe<T: 'static>(Arc<Inner<T>>);

struct Inner<T: 'static> {
    core: Core,
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

impl<T: 'static> TrickyPipe<T> {
    // TODO(eliza): we would need to add a mnemos-alloc version of this...
    pub fn new(capacity: u8) -> Self {
        Self(Arc::new(Inner {
            core: Core::new(capacity),
            elements: (0..capacity)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
        }))
    }

    const CORE_VTABLE: &'static CoreVtable = &CoreVtable {
        get_core: Self::get_core,
        get_elems: Self::get_elems,
        clone: Self::erased_clone,
        drop: Self::erased_drop,
    };

    fn erased(&self) -> ErasedPipe {
        let ptr = Arc::into_raw(self.0.clone()) as *const _;
        unsafe { ErasedPipe::new(ptr, Self::CORE_VTABLE) }
    }

    fn typed(&self) -> TypedPipe<T> {
        unsafe { self.erased().typed() }
    }

    pub fn receiver(&self) -> Option<Receiver<T>> {
        self.0.core.try_claim_rx()?;

        Some(Receiver { pipe: self.typed() })
    }

    pub fn sender(&self) -> Sender<T> {
        self.0.core.add_tx();
        Sender { pipe: self.typed() }
    }

    unsafe fn get_core(ptr: *const ()) -> *const Core {
        unsafe {
            let ptr = ptr.cast::<Inner<T>>();
            ptr::addr_of!((*ptr).core)
        }
    }

    unsafe fn get_elems(ptr: *const ()) -> ErasedSlice {
        let ptr = ptr.cast::<Inner<T>>();
        ErasedSlice::erase(&(*ptr).elements)
    }

    unsafe fn erased_clone(ptr: *const ()) {
        test_println!("erased_clone({ptr:p})");
        Arc::increment_strong_count(ptr.cast::<Inner<T>>())
    }

    unsafe fn erased_drop(ptr: *const ()) {
        let arc = Arc::from_raw(ptr.cast::<Inner<T>>());
        test_println!(refs = Arc::strong_count(&arc), "erased_drop({ptr:p})");
        drop(arc)
    }
}

impl<T: Serialize + 'static> TrickyPipe<T> {
    pub fn ser_receiver(&self) -> Option<SerReceiver> {
        self.0.core.try_claim_rx()?;

        Some(SerReceiver {
            pipe: self.erased(),
            vtable: Self::SER_VTABLE,
        })
    }

    const SER_VTABLE: &'static SerVtable = &SerVtable {
        #[cfg(any(test, feature = "alloc"))]
        to_vec: SerVtable::to_vec::<T>,
        #[cfg(any(test, feature = "alloc"))]
        to_vec_framed: SerVtable::to_vec_framed::<T>,
        to_slice: SerVtable::to_slice::<T>,
        to_slice_framed: SerVtable::to_slice_framed::<T>,
    };
}

impl<T: DeserializeOwned + 'static> TrickyPipe<T> {
    pub fn deser_sender(&self) -> DeserSender {
        self.0.core.add_tx();
        DeserSender {
            pipe: self.erased(),
            vtable: Self::DESER_VTABLE,
        }
    }

    const DESER_VTABLE: &'static DeserVtable = &DeserVtable::new::<T>();
}

unsafe impl<T: Send> Send for TrickyPipe<T> {}
unsafe impl<T: Send> Sync for TrickyPipe<T> {}

impl<T> Drop for Inner<T> {
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
