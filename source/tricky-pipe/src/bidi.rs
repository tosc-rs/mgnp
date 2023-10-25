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
}

impl SerBiDi {
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
}
