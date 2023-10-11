/// Represents a closed error
#[derive(Debug, Eq, PartialEq)]
pub enum SendError {
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TrySendError {
    Full,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    Empty,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RecvError {
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum SerSendError {
    Closed,
    Deserialize(postcard::Error),
}

#[derive(Debug, Eq, PartialEq)]
pub enum SerTrySendError {
    Send(TrySendError),
    Deserialize(postcard::Error),
}
