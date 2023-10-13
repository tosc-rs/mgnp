#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

#[cfg(any(feature = "alloc", test))]
extern crate alloc;

// TODO: only pub to silence unused warnings
pub mod spitebuf;
pub mod tp1;
pub mod tp2;

/// sorry james :P
pub mod tp3;

pub(crate) mod loom;
