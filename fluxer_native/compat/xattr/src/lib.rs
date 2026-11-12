// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forward GPUI's legacy tar dependency to the maintained xattr implementation.
//! In particular, this avoids the removed Linux libc::ENOATTR alias.
pub use xattr_current::*;
