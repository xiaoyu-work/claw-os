// SPDX-License-Identifier: GPL-3.0-only

use std::cell::UnsafeCell;
use std::ffi::CString;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::Arc;
use std::thread::ThreadId;

use claw_display_control::selection::Payload;
use claw_display_control::Error;

use crate::authority::{Authority, Origin};

struct ClawCompositor {
    owner: ThreadId,
    authority: UnsafeCell<Authority>,
}

struct ClawSelection {
    payload: Arc<Payload>,
    mimes: Vec<CString>,
}

#[derive(Default)]
#[repr(C)]
struct ClawCommand {
    serial: u64,
    token: u64,
    kind: u32,
}

fn boundary<T>(failure: T, operation: impl FnOnce() -> Result<T, Error>) -> T {
    // Rust unwinding must never cross the native compositor ABI.
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let _ = writeln!(std::io::stderr(), "Claw compositor control: {error}");
            failure
        }
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "Claw compositor control panicked");
            failure
        }
    }
}

unsafe fn with_authority<T>(
    handle: *mut ClawCompositor,
    operation: impl FnOnce(&mut Authority) -> Result<T, Error>,
) -> Result<T, Error> {
    let handle = unsafe { handle.as_ref() }.ok_or(Error::Identity)?;
    if handle.owner != std::thread::current().id() {
        return Err(Error::Protocol(
            "compositor control used outside its owning event thread",
        ));
    }
    operation(unsafe { &mut *handle.authority.get() })
}

unsafe fn text<'a>(bytes: *const u8, length: usize, maximum: usize) -> Result<&'a str, Error> {
    if bytes.is_null() || length == 0 || length > maximum {
        return Err(Error::Protocol("native text argument exceeds its bound"));
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes, length) })
        .map_err(|_| Error::Protocol("native text argument is not UTF-8"))
}

unsafe fn descriptor<'a>(fd: i32) -> Result<BorrowedFd<'a>, Error> {
    if fd < 0 {
        return Err(Error::Identity);
    }
    Ok(unsafe { BorrowedFd::borrow_raw(fd) })
}

#[no_mangle]
extern "C" fn claw_compositor_open() -> *mut ClawCompositor {
    boundary(ptr::null_mut(), || {
        let authority = Authority::open()?;
        Ok(Box::into_raw(Box::new(ClawCompositor {
            owner: std::thread::current().id(),
            authority: UnsafeCell::new(authority),
        })))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_close(handle: *mut ClawCompositor) {
    boundary((), || {
        let borrowed = unsafe { handle.as_ref() }.ok_or(Error::Identity)?;
        if borrowed.owner != std::thread::current().id() {
            return Err(Error::Protocol(
                "compositor control closed outside its owning thread",
            ));
        }
        drop(unsafe { Box::from_raw(handle) });
        Ok(())
    });
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_ready(
    handle: *mut ClawCompositor,
    display: *const u8,
    length: usize,
) -> bool {
    boundary(false, || unsafe {
        let display = text(display, length, 128)?.to_owned();
        with_authority(handle, |authority| authority.ready(display))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_next(
    handle: *mut ClawCompositor,
    command: *mut ClawCommand,
) -> i32 {
    boundary(-1, || unsafe {
        let command = command.as_mut().ok_or(Error::Identity)?;
        *command = ClawCommand::default();
        with_authority(handle, |authority| match authority.next()? {
            Some(next) => {
                *command = ClawCommand {
                    serial: next.serial,
                    token: next.token,
                    kind: next.kind as u32,
                };
                Ok(1)
            }
            None => Ok(0),
        })
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_acknowledge(
    handle: *mut ClawCompositor,
    serial: u64,
    accepted: bool,
) -> bool {
    boundary(false, || unsafe {
        with_authority(handle, |authority| authority.acknowledge(serial, accepted))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_take_descriptor(
    handle: *mut ClawCompositor,
    serial: u64,
) -> i32 {
    boundary(-1, || unsafe {
        with_authority(handle, |authority| authority.take_descriptor(serial))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_rights(
    handle: *mut ClawCompositor,
    origin: u32,
    token: u64,
) -> u32 {
    boundary(0, || unsafe {
        let origin = Origin::from_native(origin, token)?;
        with_authority(handle, |authority| authority.rights(origin))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_accepts_login(handle: *mut ClawCompositor, fd: i32) -> bool {
    boundary(false, || unsafe {
        let fd = descriptor(fd)?;
        with_authority(handle, |authority| authority.accepts_login(fd))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_client_added(
    handle: *mut ClawCompositor,
    origin: u32,
    token: u64,
    fd: i32,
) -> bool {
    boundary(false, || unsafe {
        let origin = Origin::from_native(origin, token)?;
        let fd = descriptor(fd)?;
        with_authority(handle, |authority| authority.client_added(origin, fd))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_client_removed(
    handle: *mut ClawCompositor,
    origin: u32,
    token: u64,
) -> bool {
    boundary(false, || unsafe {
        let origin = Origin::from_native(origin, token)?;
        with_authority(handle, |authority| authority.client_removed(origin))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_context_created(
    handle: *mut ClawCompositor,
    origin: u32,
    token: u64,
) -> bool {
    boundary(false, || unsafe {
        let origin = Origin::from_native(origin, token)?;
        with_authority(handle, |authority| authority.context_created(origin))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_listener_added(
    handle: *mut ClawCompositor,
    token: u64,
) -> bool {
    boundary(false, || unsafe {
        with_authority(handle, |authority| authority.listener_added(token))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_listener_removed(
    handle: *mut ClawCompositor,
    token: u64,
) -> bool {
    boundary(false, || unsafe {
        with_authority(handle, |authority| authority.listener_removed(token))?;
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_accepts_context(
    handle: *mut ClawCompositor,
    token: u64,
    fd: i32,
) -> bool {
    boundary(false, || unsafe {
        let fd = descriptor(fd)?;
        with_authority(handle, |authority| authority.accepts_context(token, fd))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_compositor_take_selection(
    handle: *mut ClawCompositor,
    serial: u64,
) -> *mut ClawSelection {
    boundary(ptr::null_mut(), || unsafe {
        let payload = with_authority(handle, |authority| authority.take_selection(serial))?;
        let mimes = payload
            .entries
            .iter()
            .map(|entry| {
                CString::new(entry.mime.as_bytes())
                    .map_err(|_| Error::Protocol("selection MIME contains a NUL"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Box::into_raw(Box::new(ClawSelection { payload, mimes })))
    })
}

#[no_mangle]
unsafe extern "C" fn claw_selection_count(selection: *const ClawSelection) -> usize {
    boundary(0, || {
        Ok(unsafe { selection.as_ref() }
            .ok_or(Error::Identity)?
            .mimes
            .len())
    })
}

#[no_mangle]
unsafe extern "C" fn claw_selection_mime(
    selection: *const ClawSelection,
    index: usize,
) -> *const libc::c_char {
    boundary(ptr::null(), || {
        Ok(unsafe { selection.as_ref() }
            .ok_or(Error::Identity)?
            .mimes
            .get(index)
            .ok_or(Error::Protocol("selection MIME index is out of bounds"))?
            .as_ptr())
    })
}

#[no_mangle]
unsafe extern "C" fn claw_selection_send(
    selection: *const ClawSelection,
    mime: *const u8,
    length: usize,
    fd: i32,
) -> bool {
    boundary(false, || unsafe {
        let selection = selection.as_ref().ok_or(Error::Identity)?;
        let mime = text(mime, length, 256)?;
        if !selection
            .payload
            .entries
            .iter()
            .any(|entry| entry.mime == mime)
        {
            return Err(Error::Protocol("selection MIME is unavailable"));
        }
        let output = descriptor(fd)?.try_clone_to_owned()?;
        selection.payload.send(mime, output);
        Ok(true)
    })
}

#[no_mangle]
unsafe extern "C" fn claw_selection_close(selection: *mut ClawSelection) {
    boundary((), || {
        if selection.is_null() {
            return Err(Error::Identity);
        }
        drop(unsafe { Box::from_raw(selection) });
        Ok(())
    });
}
