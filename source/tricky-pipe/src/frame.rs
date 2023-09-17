//! TODO: This is actually only needed once we need to be generic
//! over the kind of frames that we hold. This *will* be needed for
//! mnemos, but not the initial rp2040, which will only use RefSender
//! and RefReceiver, meaning Frames as a concept don't exist.
//!
//!
//! We may want to use `FixedVec<u8>` vs `Vec<u8>`, or maybe just always
//! require the use of FixedVec.

trait FrameHolder {
    type F: Frame;
    // TODO: async fn alloc
    fn try_alloc(&self) -> Option<Self::F>;
}

trait Frame {
    fn as_slice(&self) -> &[u8];
    fn as_mut_slice(&mut self) -> &mut [u8];
    fn set_len(&mut self, len: usize);
}

struct VecFrameHolder;

impl FrameHolder for VecFrameHolder {
    type F = VecFrame;

    fn try_alloc(&self) -> Option<Self::F> {
        Some(VecFrame { v: vec![] })
    }
}

struct VecFrame {
    v: Vec<u8>,
}

impl Frame for VecFrame {
    fn as_slice(&self) -> &[u8] {
        self.v.as_slice()
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        self.v.as_mut_slice()
    }

    fn set_len(&mut self, len: usize) {
        match self.v.len().cmp(&len) {
            cmp::Ordering::Less => {
                let diff = len - self.v.len();
                self.v.extend({
                    [0].into_iter().cycle().take(diff)
                });
            },
            cmp::Ordering::Equal => {},
            cmp::Ordering::Greater => self.v.truncate(len),
        }
    }
}
