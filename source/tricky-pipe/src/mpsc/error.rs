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
pub enum SendError<E, T = ()> {
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Disconnected(T),
    Error {
        message: T,
        error: E,
    },
}

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
pub enum TrySendError<E, T = ()> {
    /// The channel is currently full, and a message cannot be sent without
    /// waiting for a slot to become available.
    Full(T),
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Disconnected(T),
    Error {
        message: T,
        error: E,
    },
}

/// Errors returned by [`Receiver::try_recv`] and [`SerReceiver::try_recv`].
///
/// [`Receiver::try_recv`]: super::Receiver::try_recv
/// [`SerReceiver::try_recv`]: super::SerReceiver::try_recv
#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError<E> {
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
    Disconnected,
    Error(E),
}

/// Errors returned by [`Receiver::recv`] and [`SerReceiver::recv`].
///
/// [`Receiver::recv`]: super::Receiver::recv
/// [`SerReceiver::recv`]: super::SerReceiver::recv
#[derive(Debug, Eq, PartialEq)]
pub enum RecvError<E> {
    /// A message cannot be received because channel is closed.
    ///
    /// This indicates that no [`Sender`]s or [`DeserSender`]s exist, and all
    /// previously sent messages have already been received.
    ///
    /// [`Sender`]: super::Sender
    /// [`DeserSender`]: super::DeserSender
    Disconnected,
    Error(E),
}

/// Errors returned by [`DeserSender::send`] and [`DeserSender::send_framed`].
///
/// [`DeserSender::send`]: super::DeserSender::send
/// [`DeserSender::send_framed`]: super::DeserSender::send_framed
#[derive(Debug, Eq, PartialEq)]
pub enum SerSendError<E> {
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Disconnected,
    /// The sent bytes could not be deserialized to a value of this channel's
    /// message type.
    Deserialize(postcard::Error),
    Error(E),
}

/// Errors returned by [`DeserSender::try_send`] and
/// [`DeserSender::try_send_framed`].
///
/// [`DeserSender::try_send`]: super::DeserSender::try_send
/// [`DeserSender::try_send_framed`]: super::DeserSender::send_framed
#[derive(Debug, Eq, PartialEq)]
pub enum SerTrySendError<E> {
    /// A message cannot be sent because the channel is closed (no [`Receiver`]
    /// or [`SerReceiver`] exists).
    ///
    /// [`Receiver`]: super::Receiver
    /// [`SerReceiver`]: super::SerReceiver
    Disconnected,
    Full,
    /// The sent bytes could not be deserialized to a value of this channel's
    /// message type.
    Deserialize(postcard::Error),

    Error(E),
}

// === impl SendError ===

impl<E, T> SendError<E, T> {
    /// Obtain the `T` that failed to send, discarding the "kind" of [`SendError`].
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::Disconnected(msg) => msg,
            Self::Error { message, .. } => message,
        }
    }

    pub(crate) fn with_message<M>(self, message: M) -> SendError<E, M> {
        match self {
            Self::Disconnected(_) => SendError::Disconnected(message),
            Self::Error { error, .. } => SendError::Error { message, error },
        }
    }
}

impl<E: fmt::Debug, T> fmt::Debug for SendError<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(_) => f.debug_tuple("SendError::Closed").finish(),
            Self::Error { error, .. } => f
                .debug_struct("SendError::Error")
                .field("error", &error)
                .finish_non_exhaustive(),
        }
    }
}

impl<E: fmt::Display, T> fmt::Display for SendError<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(_) => f.write_str("the receiver has been dropped"),
            Self::Error { error, .. } => error.fmt(f),
        }
    }
}

// === impl TrySendError ===

impl<E, T> TrySendError<E, T> {
    /// Obtain the `T` that failed to send, discarding the "kind" of [`TrySendError`].
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::Disconnected(inner) => inner,
            Self::Full(t) => t,
            Self::Error { message, .. } => message,
        }
    }

    pub(crate) fn with_message<M>(self, message: M) -> TrySendError<E, M> {
        match self {
            Self::Disconnected(_) => TrySendError::Disconnected(message),
            Self::Full(_) => TrySendError::Full(message),
            Self::Error { error, .. } => TrySendError::Error { message, error },
        }
    }
}

impl<E: fmt::Debug, T> fmt::Debug for TrySendError<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(_) => f.debug_tuple("TrySendError::Closed").finish(),
            Self::Full(_) => f.debug_tuple("TrySendError::Full").finish(),
            Self::Error { error, .. } => f
                .debug_struct("TrySendError::Error")
                .field("error", &error)
                .finish_non_exhaustive(),
        }
    }
}

impl<E: fmt::Display, T> fmt::Display for TrySendError<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(_) => f.write_str("the receiver has been dropped"),
            Self::Full(_) => f.write_str("the channel is currently at capacity"),
            Self::Error { error, .. } => error.fmt(f),
        }
    }
}

// === impl SerSendError ===

impl<E> SerSendError<E> {
    pub(crate) fn from_send_error(err: SendError<E>) -> Self {
        match err {
            SendError::Disconnected(_) => Self::Disconnected,
            SendError::Error { error, .. } => Self::Error(error),
        }
    }
}

impl<E: fmt::Display> fmt::Display for SerSendError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => f.write_str("the receiver has been dropped"),
            Self::Error(error) => error.fmt(f),
            Self::Deserialize(error) => write!(f, "error deserializing message: {error}"),
        }
    }
}

// === impl SerTrySendError ===

impl<E> SerTrySendError<E> {
    pub(crate) fn from_try_send_error(err: TrySendError<E>) -> Self {
        match err {
            TrySendError::Disconnected(_) => Self::Disconnected,
            TrySendError::Error { error, .. } => Self::Error(error),
            TrySendError::Full(_) => Self::Full,
        }
    }

    pub(crate) fn from_ser_send_error(err: SerSendError<E>) -> Self {
        match err {
            SerSendError::Disconnected => Self::Disconnected,
            SerSendError::Error(e) => Self::Error(e),
            SerSendError::Deserialize(e) => Self::Deserialize(e),
        }
    }
}

impl<E: fmt::Display> fmt::Display for SerTrySendError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => f.write_str("the receiver has been dropped"),
            Self::Error(error) => error.fmt(f),
            Self::Full => f.write_str("the channel is currently at capacity"),
            Self::Deserialize(error) => write!(f, "error deserializing message: {error}"),
        }
    }
}
