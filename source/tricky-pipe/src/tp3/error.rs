//! Error types returned by [`Sender`]s, [`Receiver`]s, [`SerSender`]s, and
//! [`SerReceiver`]s.
//!
//! [`Sender`]: super::Sender
//! [`Receiver`]: super::Receiver
//! [`SerSender`]: super::SerSender
//! [`SerReceiver`]: super::SerReceiver

/// Error returned by [`Sender::reserve`](super::Sender::reserve).
#[derive(Debug, Eq, PartialEq)]
pub enum SendError {
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Sender`]: super::Sender
    /// [`Receiver`]: super::Receiver
    Closed,
}

/// Error returned by [`Sender::try_reserve`](super::Sender::try_reserve).
#[derive(Debug, Eq, PartialEq)]
pub enum TrySendError {
    /// The channel is currently full, and a message cannot be sent without
    /// waiting for a slot to become available.
    Full,
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Sender`]: super::Sender
    /// [`Receiver`]: super::Receiver
    Closed,
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
    /// This indicates that no [`Sender`]s or [`SerSender`]s exist, and all
    /// previously sent messages have already been received.
    ///
    /// [`Sender`]: super::Sender
    /// [`SerSender`]: super::SerSender
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
    /// This indicates that no [`Sender`]s or [`SerSender`]s exist, and all
    /// previously sent messages have already been received.
    ///
    /// [`Sender`]: super::Sender
    /// [`SerSender`]: super::SerSender
    Closed,
}

/// Errors returned by [`SerSender::send`] and [`SerSender::send_framed`].
///
/// [`SerSender::send`]: super::SerSender::send
/// [`SerSender::send_framed`]: super::SerSender::send_framed
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

/// Errors returned by [`SerSender::try_send`] and
/// [`SerSender::try_send_framed`].
///
/// [`SerSender::try_send`]: super::SerSender::try_send
/// [`SerSender::try_send_framed`]: super::SerSender::send_framed
#[derive(Debug, Eq, PartialEq)]
pub enum SerTrySendError {
    /// The channel is [`Closed`](TrySendError::Closed) or [`Full`](TrySendError::Full).
    Send(TrySendError),
    /// The sent bytes could not be deserialized to a value of this channel's
    /// message type.
    Deserialize(postcard::Error),
}
