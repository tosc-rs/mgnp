//! A basic test making an `Arc<Mutex<[T]>>` with a plugtail
//!
//! Not meant to be used, just for testing the underlying structure out.

use std::{sync::atomic::{AtomicBool, Ordering, AtomicU8}, marker::PhantomData, ptr::drop_in_place, mem::MaybeUninit, ops::{Deref, DerefMut}};

use plugtail::{Pluggable, BodyDrop, alloc::ArcPlugTail, PlugDat, StaticPlugTail};

//- ACTUAL IMPLEMENTATION STUFF ----
//
// The goal is that the stuff in THIS region is *exactly the same*
// regardless of whether the storage is static or arc.

mod sealed {
    use super::*;
    pub struct AmsHdr<T> {
        pub(crate) locked: AtomicBool,
        pub(crate) pd: PhantomData<fn() -> T>,
    }
}

use sealed::AmsHdr;

#[derive(Clone)]
pub struct Ams<T, P>
where
    P: Pluggable<Header = AmsHdr<T>, Item = T>,
{
    pt: P,
}

pub type ArcAms<T> = Ams<T, ArcPlugTail<AmsHdr<T>, T>>;
pub type StaticAms<T, const N: usize> = Ams<T, &'static StaticPlugTail<AmsHdr<T>, T, N>>;

pub struct AmsGuard<'a, T>
{
    dat: PlugDat<'a, AmsHdr<T>, T>,
}

impl<'a, T> Deref for AmsGuard<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        let len = self.dat.t.len();
        let ptr: *const T = self.dat.t.as_ptr().cast();
        unsafe {
            core::slice::from_raw_parts(ptr, len)
        }
    }
}

impl<'a, T> Drop for AmsGuard<'a, T> {
    fn drop(&mut self) {
        self.dat.hdr.locked.store(false, Ordering::Release)
    }
}

impl<'a, T> DerefMut for AmsGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let len = self.dat.t.len();
        let ptr: *const T = self.dat.t.as_ptr().cast();
        let ptr: *mut T = ptr.cast_mut();
        unsafe {
            core::slice::from_raw_parts_mut(ptr, len)
        }
    }
}


impl<T, P> Ams<T, P>
where
    P: Pluggable<Header = AmsHdr<T>, Item = T>,
{
    pub fn try_lock(&self) -> Option<AmsGuard<'_, T>> {
        let dat = self.pt.storage();
        if dat.hdr.locked.swap(true, Ordering::AcqRel) {
            // already locked, sorry
            return None;
        }
        // okay we have exclusive access!
        Some(AmsGuard { dat })
    }
}

impl<T> Ams<T, ArcPlugTail<AmsHdr<T>, T>>
{
    pub fn new_arc_with<W: Fn() -> T>(n: usize, f: W) -> Self {
        let h: AmsHdr<T> = AmsHdr { locked: AtomicBool::new(false), pd: PhantomData };
        let apt = ArcPlugTail::new(h, n);
        let sto = apt.storage();
        for val in sto.t {
            unsafe {
                val.get().write(MaybeUninit::new(f()));
            }
        }
        Self {
            pt: apt,
        }
    }
}

impl<T> BodyDrop for AmsHdr<T> {
    type Item = T;
    fn body_drop(&self, i: &[std::cell::UnsafeCell<std::mem::MaybeUninit<Self::Item>>]) {
        // In Ams, all items are initialized and valid, so always drop them all.
        for val in i {
            unsafe {
                let p: *mut T = val.get().cast();
                drop_in_place(p);
            }
        }
    }
}

//
// End of "exactly the same" implementation stuff.

//- IMPLEMENTATION STUFF: Static storage ---
//
// The reason these details exist is that it is often advantageous to have
// a specific runtime init for statics, as it means that we can leave more
// of the contents `uninit` or zero, meaning that these static only exist
// in .bss, and NOT in .data, meaning we don't need (sometimes huge!)
// initialization values.
//
// Some if this wouldn't be necessary if you didn't have the invariant that
// "the slice is always valid" like [Ams] does.

pub struct StaticAmsStorage<T, const N: usize> {
    is_init: AtomicU8,
    sp: StaticPlugTail<AmsHdr<T>, T, N>,
}

// TODO: should StaticPlugTail have any kind of blanket impl for
// Send or Sync?
unsafe impl<T: Send, const N: usize> Send for StaticAmsStorage<T, N> { }
unsafe impl<T: Send, const N: usize> Sync for StaticAmsStorage<T, N> { }

impl<T, const N: usize> StaticAmsStorage<T, N> {
    const UNINIT: u8 = 0;
    const INITING: u8 = 1;
    const INITED: u8 = 2;

    pub const fn new() -> Self {
        Self {
            is_init: AtomicU8::new(Self::UNINIT),
            sp: StaticPlugTail::uninit(AmsHdr { locked: AtomicBool::new(false), pd: PhantomData }),
        }
    }

    pub fn try_init_take<W: Fn() -> T>(&'static self, f: W) -> Option<Ams<T, &'static StaticPlugTail<AmsHdr<T>, T, N>>> {
        let val = self.is_init.compare_exchange(Self::UNINIT, Self::INITING, Ordering::AcqRel, Ordering::Acquire);
        match val {
            Ok(_) => {},
            Err(Self::INITED) => {
                // Already initialized, good2go
                return Some(Ams { pt: &self.sp });
            },
            Err(Self::INITING) => return None,
            _ => {
                // It wasn't UNINIT, INITING, or INITED, what was it?
                debug_assert!(false, "What? {val:?}");
                return None;
            }
        }

        // We now have exclusive init'ing access
        let spref = &self.sp;
        let sto = spref.storage();
        for val in sto.t {
            unsafe {
                val.get().write(MaybeUninit::new(f()));
            }
        }
        self.is_init.store(Self::INITED, Ordering::Release);
        Some(Ams { pt: spref })
    }

    pub fn try_take(&'static self) -> Option<Ams<T, &'static StaticPlugTail<AmsHdr<T>, T, N>>> {
        let val = self.is_init.load(Ordering::Acquire);
        if val != Self::INITED {
            None
        } else {
            Some(Ams { pt: &self.sp })
        }
    }
}

//- TESTING STUFF: ARC MODE ----

#[test]
fn make_a_thing() {
    let x: ArcAms<i32> = Ams::new_arc_with(10, || 123);

    // lock once, read values
    {
        let g = x.try_lock().unwrap();

        let mut ctr = 0;
        for v in g.deref() {
            assert_eq!(*v, 123);
            ctr += 1;
        }
        assert_eq!(ctr, 10);
    }

    // second time, set values
    {
        let mut g = x.try_lock().unwrap();

        for (i, v) in g.deref_mut().iter_mut().enumerate() {
            *v = i as i32;
        }
    }

    // third time, read values
    {
        let g = x.try_lock().unwrap();

        for (i, v) in g.deref().iter().enumerate() {
            assert_eq!(*v, i as i32);
        }
    }

}

#[test]
fn lock_works() {
    let x: ArcAms<i32> = Ams::new_arc_with(10, || 123);

    // lock once, hold it
    let g1 = x.try_lock().unwrap();

    // Can't lock while holding
    assert!(x.try_lock().is_none());

    // Use the lock
    assert!(g1.deref().iter().all(|v| *v == 123));
}

#[test]
fn arc_works() {
    let x1: ArcAms<i32> = Ams::new_arc_with(10, || 123);
    let x2 = x1.clone();

    // modify first one
    {
        let mut g = x1.try_lock().unwrap();

        for (i, v) in g.deref_mut().iter_mut().enumerate() {
            *v = i as i32;
        }
    }

    // drop first one
    drop(x1);

    // read via second
    {
        let g = x2.try_lock().unwrap();

        for (i, v) in g.deref().iter().enumerate() {
            assert_eq!(*v, i as i32);
        }
    }
}

//- TESTING STUFF: STATIC MODE ----

#[test]
fn static_works() {
    static AMS_STO: StaticAmsStorage<i32, 16> = StaticAmsStorage::new();

    // Can't take without init
    assert!(AMS_STO.try_take().is_none());

    // Init works
    let x1: StaticAms<i32, 16> = AMS_STO.try_init_take(|| 123).unwrap();

    // Second init works, but never calls init fn
    let x2: StaticAms<i32, 16> = AMS_STO.try_init_take(|| panic!()).unwrap();

    // Clone works - equivalent to second init-take
    let x3 = x1.clone();

    // modify first one
    {
        let mut g = x1.try_lock().unwrap();

        for (i, v) in g.deref_mut().iter_mut().enumerate() {
            *v = i as i32;
        }
    }

    // drop first one
    #[allow(clippy::drop_non_drop)]
    drop(x1);

    // read via second
    {
        let g = x2.try_lock().unwrap();

        for (i, v) in g.deref().iter().enumerate() {
            assert_eq!(*v, i as i32);
        }
    }

    #[allow(clippy::drop_non_drop)]
    drop(x2);

    // read via third
    {
        let g = x3.try_lock().unwrap();

        for (i, v) in g.deref().iter().enumerate() {
            assert_eq!(*v, i as i32);
        }
    }
}
