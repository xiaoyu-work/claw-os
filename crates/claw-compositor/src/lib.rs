// SPDX-License-Identifier: GPL-3.0-only

//! OS-private compositor enforcement shared with the native Hyprland adapter.

#![cfg(target_os = "linux")]
#![deny(unsafe_op_in_unsafe_fn)]

mod authority;
mod ffi;
