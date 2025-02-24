#![allow(non_camel_case_types)]
#![allow(unused_unsafe)]
use crate::old::snapshot_0::hostcalls_impl::{ClockEventData, FdEventData};
use crate::old::snapshot_0::sys::host_impl;
use crate::old::snapshot_0::{wasi, Error, Result};
use nix::libc::{self, c_int};
use std::mem::MaybeUninit;

fn wasi_clock_id_to_unix(clock_id: wasi::__wasi_clockid_t) -> Result<libc::clockid_t> {
    // convert the supported clocks to the libc types, or return EINVAL
    match clock_id {
        wasi::__WASI_CLOCKID_REALTIME => Ok(libc::CLOCK_REALTIME),
        wasi::__WASI_CLOCKID_MONOTONIC => Ok(libc::CLOCK_MONOTONIC),
        wasi::__WASI_CLOCKID_PROCESS_CPUTIME_ID => Ok(libc::CLOCK_PROCESS_CPUTIME_ID),
        wasi::__WASI_CLOCKID_THREAD_CPUTIME_ID => Ok(libc::CLOCK_THREAD_CPUTIME_ID),
        _ => Err(Error::EINVAL),
    }
}

pub(crate) fn clock_res_get(clock_id: wasi::__wasi_clockid_t) -> Result<wasi::__wasi_timestamp_t> {
    let clock_id = wasi_clock_id_to_unix(clock_id)?;
    // no `nix` wrapper for clock_getres, so we do it ourselves
    let mut timespec = MaybeUninit::<libc::timespec>::uninit();
    let res = unsafe { libc::clock_getres(clock_id, timespec.as_mut_ptr()) };
    if res != 0 {
        return Err(host_impl::errno_from_nix(nix::errno::Errno::last()));
    }
    let timespec = unsafe { timespec.assume_init() };

    // convert to nanoseconds, returning EOVERFLOW in case of overflow;
    // this is freelancing a bit from the spec but seems like it'll
    // be an unusual situation to hit
    (timespec.tv_sec as wasi::__wasi_timestamp_t)
        .checked_mul(1_000_000_000)
        .and_then(|sec_ns| sec_ns.checked_add(timespec.tv_nsec as wasi::__wasi_timestamp_t))
        .map_or(Err(Error::EOVERFLOW), |resolution| {
            // a supported clock can never return zero; this case will probably never get hit, but
            // make sure we follow the spec
            if resolution == 0 {
                Err(Error::EINVAL)
            } else {
                Ok(resolution)
            }
        })
}

pub(crate) fn clock_time_get(clock_id: wasi::__wasi_clockid_t) -> Result<wasi::__wasi_timestamp_t> {
    let clock_id = wasi_clock_id_to_unix(clock_id)?;
    // no `nix` wrapper for clock_getres, so we do it ourselves
    let mut timespec = MaybeUninit::<libc::timespec>::uninit();
    let res = unsafe { libc::clock_gettime(clock_id, timespec.as_mut_ptr()) };
    if res != 0 {
        return Err(host_impl::errno_from_nix(nix::errno::Errno::last()));
    }
    let timespec = unsafe { timespec.assume_init() };

    // convert to nanoseconds, returning EOVERFLOW in case of overflow; this is freelancing a bit
    // from the spec but seems like it'll be an unusual situation to hit
    (timespec.tv_sec as wasi::__wasi_timestamp_t)
        .checked_mul(1_000_000_000)
        .and_then(|sec_ns| sec_ns.checked_add(timespec.tv_nsec as wasi::__wasi_timestamp_t))
        .map_or(Err(Error::EOVERFLOW), Ok)
}

pub(crate) fn poll_oneoff(
    timeout: Option<ClockEventData>,
    fd_events: Vec<FdEventData>,
    events: &mut Vec<wasi::__wasi_event_t>,
) -> Result<()> {
    use nix::{
        errno::Errno,
        poll::{poll, PollFd, PollFlags},
    };
    use std::{convert::TryInto, os::unix::prelude::AsRawFd};

    if fd_events.is_empty() && timeout.is_none() {
        return Ok(());
    }

    let mut poll_fds: Vec<_> = fd_events
        .iter()
        .map(|event| {
            let mut flags = PollFlags::empty();
            match event.r#type {
                wasi::__WASI_EVENTTYPE_FD_READ => flags.insert(PollFlags::POLLIN),
                wasi::__WASI_EVENTTYPE_FD_WRITE => flags.insert(PollFlags::POLLOUT),
                // An event on a file descriptor can currently only be of type FD_READ or FD_WRITE
                // Nothing else has been defined in the specification, and these are also the only two
                // events we filtered before. If we get something else here, the code has a serious bug.
                _ => unreachable!(),
            };
            PollFd::new(event.descriptor.as_raw_fd(), flags)
        })
        .collect();

    let poll_timeout = timeout.map_or(-1, |timeout| {
        let delay = timeout.delay / 1_000_000; // poll syscall requires delay to expressed in milliseconds
        delay.try_into().unwrap_or(c_int::max_value())
    });
    log::debug!("poll_oneoff poll_timeout = {:?}", poll_timeout);

    let ready = loop {
        match poll(&mut poll_fds, poll_timeout) {
            Err(_) => {
                if Errno::last() == Errno::EINTR {
                    continue;
                }
                return Err(host_impl::errno_from_nix(Errno::last()));
            }
            Ok(ready) => break ready as usize,
        }
    };

    Ok(if ready == 0 {
        poll_oneoff_handle_timeout_event(timeout.expect("timeout should not be None"), events)
    } else {
        let ready_events = fd_events.into_iter().zip(poll_fds.into_iter()).take(ready);
        poll_oneoff_handle_fd_event(ready_events, events)?
    })
}

// define the `fionread()` function, equivalent to `ioctl(fd, FIONREAD, *bytes)`
nix::ioctl_read_bad!(fionread, nix::libc::FIONREAD, c_int);

fn poll_oneoff_handle_timeout_event(
    timeout: ClockEventData,
    events: &mut Vec<wasi::__wasi_event_t>,
) {
    events.push(wasi::__wasi_event_t {
        userdata: timeout.userdata,
        r#type: wasi::__WASI_EVENTTYPE_CLOCK,
        error: wasi::__WASI_ERRNO_SUCCESS,
        u: wasi::__wasi_event_u_t {
            fd_readwrite: wasi::__wasi_event_fd_readwrite_t {
                nbytes: 0,
                flags: 0,
            },
        },
    });
}

fn poll_oneoff_handle_fd_event<'a>(
    ready_events: impl Iterator<Item = (FdEventData<'a>, nix::poll::PollFd)>,
    events: &mut Vec<wasi::__wasi_event_t>,
) -> Result<()> {
    use nix::poll::PollFlags;
    use std::{convert::TryInto, os::unix::prelude::AsRawFd};

    for (fd_event, poll_fd) in ready_events {
        log::debug!("poll_oneoff_handle_fd_event fd_event = {:?}", fd_event);
        log::debug!("poll_oneoff_handle_fd_event poll_fd = {:?}", poll_fd);

        let revents = match poll_fd.revents() {
            Some(revents) => revents,
            None => continue,
        };

        log::debug!("poll_oneoff_handle_fd_event revents = {:?}", revents);

        let mut nbytes = 0;
        if fd_event.r#type == wasi::__WASI_EVENTTYPE_FD_READ {
            let _ = unsafe { fionread(fd_event.descriptor.as_raw_fd(), &mut nbytes) };
        }

        let output_event = if revents.contains(PollFlags::POLLNVAL) {
            wasi::__wasi_event_t {
                userdata: fd_event.userdata,
                r#type: fd_event.r#type,
                error: wasi::__WASI_ERRNO_BADF,
                u: wasi::__wasi_event_u_t {
                    fd_readwrite: wasi::__wasi_event_fd_readwrite_t {
                        nbytes: 0,
                        flags: wasi::__WASI_EVENTRWFLAGS_FD_READWRITE_HANGUP,
                    },
                },
            }
        } else if revents.contains(PollFlags::POLLERR) {
            wasi::__wasi_event_t {
                userdata: fd_event.userdata,
                r#type: fd_event.r#type,
                error: wasi::__WASI_ERRNO_IO,
                u: wasi::__wasi_event_u_t {
                    fd_readwrite: wasi::__wasi_event_fd_readwrite_t {
                        nbytes: 0,
                        flags: wasi::__WASI_EVENTRWFLAGS_FD_READWRITE_HANGUP,
                    },
                },
            }
        } else if revents.contains(PollFlags::POLLHUP) {
            wasi::__wasi_event_t {
                userdata: fd_event.userdata,
                r#type: fd_event.r#type,
                error: wasi::__WASI_ERRNO_SUCCESS,
                u: wasi::__wasi_event_u_t {
                    fd_readwrite: wasi::__wasi_event_fd_readwrite_t {
                        nbytes: 0,
                        flags: wasi::__WASI_EVENTRWFLAGS_FD_READWRITE_HANGUP,
                    },
                },
            }
        } else if revents.contains(PollFlags::POLLIN) | revents.contains(PollFlags::POLLOUT) {
            wasi::__wasi_event_t {
                userdata: fd_event.userdata,
                r#type: fd_event.r#type,
                error: wasi::__WASI_ERRNO_SUCCESS,
                u: wasi::__wasi_event_u_t {
                    fd_readwrite: wasi::__wasi_event_fd_readwrite_t {
                        nbytes: nbytes.try_into()?,
                        flags: 0,
                    },
                },
            }
        } else {
            continue;
        };

        events.push(output_event);
    }

    Ok(())
}
