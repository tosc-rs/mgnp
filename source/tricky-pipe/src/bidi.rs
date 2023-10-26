#![allow(missing_docs)]
use super::*;
use futures::FutureExt;

pub struct BiDi<In: 'static, Out: 'static = In> {
    tx: Sender<Out>,
    rx: Receiver<In>,
}

pub struct SerBiDi {
    tx: DeserSender,
    rx: SerReceiver,
}

pub enum Event<In, Out> {
    Recv(In),
    SendReady(Out),
}

impl<In, Out> BiDi<In, Out>
where
    In: 'static,
    Out: 'static,
{
    #[must_use]
    pub fn new(tx: Sender<Out>, rx: Receiver<In>) -> Self {
        Self { tx, rx }
    }

    #[must_use]
    pub fn split(self) -> (Sender<Out>, Receiver<In>) {
        (self.tx, self.rx)
    }

    #[must_use]
    pub async fn wait(&self) -> Option<Event<In, Permit<'_, Out>>> {
        futures::select_biased! {
            res = self.tx.reserve().fuse() => {
                match res {
                    Ok(permit) => Some(Event::SendReady(permit)),
                    Err(_) => self.rx.recv().await.map(Event::Recv),
                }
            }
            recv = self.rx.recv().fuse() => {
                recv.map(Event::Recv)
            }
        }
    }

    /// Borrows the **send half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`Sender::send`], [`Sender::reserve`],
    /// [`Sender::try_reserve`], [`Sender::capacity`], et cetera, on the send
    /// half of the channel.
    #[must_use]
    pub fn tx(&self) -> &Sender<Out> {
        &self.tx
    }

    /// Borrows the **receive half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`Receiver::recv`],
    /// [`Receiver::try_recv`], [`Receiver::capacity`], et cetera, on the
    /// receive half of the channel.
    #[must_use]
    pub fn rx(&self) -> &Receiver<In> {
        &self.rx
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// empty.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are empty. To check if only one the send or
    /// receive half is empty, use
    /// [`self.tx()`]`.`[`is_empty()`](Sender::is_empty) or
    /// [`self.rx()`]`.`[`is_empty()`](Receiver::is_empty), respectively.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty() && self.rx.is_empty()
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// full.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are full. To check if only one the send or
    /// receive half is full, use
    /// [`self.tx()`]`.`[`is_full()`](Sender::is_full) or
    /// [`self.rx()`]`.`[`is_full()`](Receiver::is_full), respectively.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.tx.is_full() && self.rx.is_full()
    }
}

impl SerBiDi {
    #[must_use]
    pub fn new(tx: DeserSender, rx: SerReceiver) -> Self {
        Self { tx, rx }
    }

    #[must_use]
    pub fn split(self) -> (DeserSender, SerReceiver) {
        (self.tx, self.rx)
    }

    #[must_use]
    pub async fn wait(&self) -> Option<Event<SerRecvRef<'_>, SerPermit<'_>>> {
        futures::select_biased! {
            res = self.tx.reserve().fuse() => {
                match res {
                    Ok(permit) => Some(Event::SendReady(permit)),
                    Err(_) => self.rx.recv().await.map(Event::Recv),
                }
            }
            recv = self.rx.recv().fuse() => {
                recv.map(Event::Recv)
            }
        }
    }

    /// Borrows the **send half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`DeserSender::reserve`],
    /// [`DeserSender::try_reserve`], [`DeserSender::capacity`], et cetera, on
    /// the send half of the channel.
    #[must_use]
    pub fn tx(&self) -> &DeserSender {
        &self.tx
    }

    /// Borrows the **receive half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`SerReceiver::recv`],
    /// [`SerReceiver::try_recv`], [`SerReceiver::capacity`], et cetera, on the
    /// receive half of the channel.
    #[must_use]
    pub fn rx(&self) -> &SerReceiver {
        &self.rx
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// empty.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are empty. To check if only one the send or
    /// receive half is empty, use
    /// [`self.tx()`]`.`[`is_empty()`](DeserSender::is_empty) or
    /// [`self.rx()`]`.`[`is_empty()`](SerReceiver::is_empty), respectively.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty() && self.rx.is_empty()
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// full.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are full. To check if only one the send or
    /// receive half is full, use
    /// [`self.tx()`]`.`[`is_full()`](DeserSender::is_full) or
    /// [`self.rx()`]`.`[`is_full()`](SerReceiver::is_full), respectively.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.tx.is_full() && self.rx.is_full()
    }
}
