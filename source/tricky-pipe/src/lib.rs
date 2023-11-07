//! Tricky Pipe
//!
//! Tricky Pipe is a channel that has interchangeable ends to allow for
//! transparent serialization and deserialization.
//!
//! It is intended to be used in cases where you *sometimes* need to traverse
//! a network hop (or similar), and Serialization or Deserialization may occur.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![warn(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]

#[cfg(any(feature = "alloc", test, loom))]
extern crate alloc;

#[cfg(not(test))]
macro_rules! test_dbg {
    ($x:expr) => {
        $x
    };
}

#[cfg(test)]
macro_rules! test_dbg {
    ($x:expr) => {
        match $x {
            x => {
                const EXPR: &str = stringify!($x);
                tracing::event!(tracing::Level::DEBUG, { EXPR } = ?format_args!("{x:#?}"));
                x
            }
        }
    };
}

#[cfg(not(test))]
macro_rules! test_println {
    ($($arg:tt)*) => {};
}

#[cfg(test)]
macro_rules! test_println {
    ($($arg:tt)*) => {
        tracing::info!($($arg)*);
    };
}

#[cfg(not(test))]
macro_rules! test_span {
    ($($arg:tt)*) => {};
}

#[cfg(all(test, not(loom)))]
macro_rules! test_span {
    ($($arg:tt)*) => {
        let _span = tracing::debug_span!($($arg)*).entered();
    };
}

#[cfg(all(test, loom))]
macro_rules! test_span {
    ($($arg:tt)*) => {
        tracing::info!(message = $($arg)*);
    };
}

pub mod bidi;
pub(crate) mod loom;
pub mod mpsc;
pub mod oneshot;
pub mod serbox;
pub(crate) mod typeinfo;
