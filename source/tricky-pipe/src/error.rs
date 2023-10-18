//! Error types returned by [`Sender`]s, [`Receiver`]s, [`DeserSender`]s, and
//! [`SerReceiver`]s.
//!
//! [`Sender`]: super::Sender
//! [`Receiver`]: super::Receiver
//! [`DeserSender`]: super::DeserSender
//! [`SerReceiver`]: super::SerReceiver
use core::fmt;

/// A message cannot be sent because the channel is closed (no [`Receiver`]
/// or [`SerReceiver`] exists).
///
/// A `SendError<()>` is returned by the [`Sender::reserve`] and
/// [`DeserSender::reserve`] methods. The [`Sender::send`] method instead returns
/// a `SendError<T>`, from which the original message can be recovered using
/// [`SendError::into_inner`].
///
/// [`Receiver`]: super::Receiver
/// [`SerReceiver`]: super::SerReceiver
/// [`Sender::reserve`]: super::Sender::reserve
/// [`DeserSender::reserve`]: super::DeserSender::reserve
/// [`Sender::send`]: super::Sender::send
#[derive(Eq, PartialEq)]
pub struct SendError<T = ()>(pub(crate) T);

/// Error returned by [`Sender::try_reserve`], [`Sender::try_send`], and
/// [`DeserSender::try_reserve`].
///
/// A `TrySendError<()>` is returned by the [`Sender::try_reserve`] and
/// [`DeserSender::try_reserve`] methods. The [`Sender::try_send`] method instead
/// returns a `TrySendError<T>`, from which the original message can be
/// recovered using [`TrySendError::into_inner`].
///
/// [`Sender::try_reserve`]: super::Sender::try_reserve
/// [`DeserSender::try_reserve`]: super::DeserSender::try_reserve
/// [`Sender::try_send`]: super::Sender::try_send
#[derive(Eq, PartialEq)]
pub enum TrySendError<T = ()> {
    /// The channel is currently full, and a message cannot be sent without
    /// waiting for a slot to become available.
    Full(T),
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Closed(T),
}

/// Errors returned by [`Receiver::try_recv`] and [`SerReceiver::try_recv`].
///
/// [`Receiver::try_recv`]: super::Receiver::try_recv
/// [`SerReceiver::try_recv`]: super::SerReceiver::try_recv
#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    /// No messages are currently present in the channel. The receiver must wait
    /// for an additional message to be sent.
    Empty,
    /// A message cannot be received because the channel is closed.
    ///
    /// This indicates that no [`Sender`]s or [`DeserSender`]s exist, and all
    /// previously sent messages have already been received.
    ///
    /// [`Sender`]: super::Sender
    /// [`DeserSender`]: super::DeserSender
    Closed,
}

/// Errors returned by [`Receiver::recv`] and [`SerReceiver::recv`].
///
/// [`Receiver::recv`]: super::Receiver::recv
/// [`SerReceiver::recv`]: super::SerReceiver::recv
#[derive(Debug, Eq, PartialEq)]
pub enum RecvError {
    /// A message cannot be received because channel is closed.
    ///
    /// This indicates that no [`Sender`]s or [`DeserSender`]s exist, and all
    /// previously sent messages have already been received.
    ///
    /// [`Sender`]: super::Sender
    /// [`DeserSender`]: super::DeserSender
    Closed,
}

/// Errors returned by [`DeserSender::send`] and [`DeserSender::send_framed`].
///
/// [`DeserSender::send`]: super::DeserSender::send
/// [`DeserSender::send_framed`]: super::DeserSender::send_framed
#[derive(Debug, Eq, PartialEq)]
pub enum SerSendError {
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Closed,
    /// The sent bytes could not be deserialized to a value of this channel's
    /// message type.
    Deserialize(postcard::Error),
}

/// Errors returned by [`DeserSender::try_send`] and
/// [`DeserSender::try_send_framed`].
///
/// [`DeserSender::try_send`]: super::DeserSender::try_send
/// [`DeserSender::try_send_framed`]: super::DeserSender::send_framed
#[derive(Debug, Eq, PartialEq)]
pub enum SerTrySendError {
    /// The channel is [`Closed`](TrySendError::Closed) or [`Full`](TrySendError::Full).
    Send(TrySendError),
    /// The sent bytes could not be deserialized to a value of this channel's
    /// message type.
    Deserialize(postcard::Error),
}

// === impl SendError ===

impl<T> SendError<T> {
    /// Obtain the `T` that failed to send, discarding the "kind" of [SendError].
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish_non_exhaustive()
    }
}

// === impl TrySendError ===

impl<T> TrySendError<T> {
    /// Obtain the `T` that failed to send, discarding the "kind" of [TrySendError].
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            TrySendError::Closed(t) => t,
            TrySendError::Full(t) => t,
        }
    }
}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(_) => f.write_str("Closed(..)"),
            Self::Full(_) => f.write_str("Full(..)"),
        }
    }
}
