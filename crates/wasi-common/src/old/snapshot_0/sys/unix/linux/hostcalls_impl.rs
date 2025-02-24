use super::super::dir::{Dir, Entry, SeekLoc};
use crate::old::snapshot_0::hostcalls_impl::{Dirent, PathGet};
use crate::old::snapshot_0::sys::host_impl;
use crate::old::snapshot_0::sys::unix::str_to_cstring;
use crate::old::snapshot_0::{wasi, Error, Result};
use log::trace;
use std::convert::TryInto;
use std::fs::File;
use std::os::unix::prelude::AsRawFd;

pub(crate) fn path_unlink_file(resolved: PathGet) -> Result<()> {
    use nix::errno;
    use nix::libc::unlinkat;

    let path_cstr = str_to_cstring(resolved.path())?;

    // nix doesn't expose unlinkat() yet
    let res = unsafe { unlinkat(resolved.dirfd().as_raw_fd(), path_cstr.as_ptr(), 0) };
    if res == 0 {
        Ok(())
    } else {
        Err(host_impl::errno_from_nix(errno::Errno::last()))
    }
}

pub(crate) fn path_symlink(old_path: &str, resolved: PathGet) -> Result<()> {
    use nix::{errno::Errno, libc::symlinkat};

    let old_path_cstr = str_to_cstring(old_path)?;
    let new_path_cstr = str_to_cstring(resolved.path())?;

    log::debug!("path_symlink old_path = {:?}", old_path);
    log::debug!("path_symlink resolved = {:?}", resolved);

    let res = unsafe {
        symlinkat(
            old_path_cstr.as_ptr(),
            resolved.dirfd().as_raw_fd(),
            new_path_cstr.as_ptr(),
        )
    };
    if res != 0 {
        Err(host_impl::errno_from_nix(Errno::last()))
    } else {
        Ok(())
    }
}

pub(crate) fn path_rename(resolved_old: PathGet, resolved_new: PathGet) -> Result<()> {
    use nix::libc::renameat;
    let old_path_cstr = str_to_cstring(resolved_old.path())?;
    let new_path_cstr = str_to_cstring(resolved_new.path())?;

    let res = unsafe {
        renameat(
            resolved_old.dirfd().as_raw_fd(),
            old_path_cstr.as_ptr(),
            resolved_new.dirfd().as_raw_fd(),
            new_path_cstr.as_ptr(),
        )
    };
    if res != 0 {
        Err(host_impl::errno_from_nix(nix::errno::Errno::last()))
    } else {
        Ok(())
    }
}

pub(crate) fn fd_readdir(
    fd: &File,
    cookie: wasi::__wasi_dircookie_t,
) -> Result<impl Iterator<Item = Result<Dirent>>> {
    // We need to duplicate the fd, because `opendir(3)`:
    //     After a successful call to fdopendir(), fd is used internally by the implementation,
    //     and should not otherwise be used by the application.
    // `opendir(3p)` also says that it's undefined behavior to
    // modify the state of the fd in a different way than by accessing DIR*.
    //
    // Still, rewinddir will be needed because the two file descriptors
    // share progress. But we can safely execute closedir now.
    let fd = fd.try_clone()?;
    let mut dir = Dir::from(fd)?;

    // Seek if needed. Unless cookie is wasi::__WASI_DIRCOOKIE_START,
    // new items may not be returned to the caller.
    //
    // According to `opendir(3p)`:
    //     If a file is removed from or added to the directory after the most recent call
    //     to opendir() or rewinddir(), whether a subsequent call to readdir() returns an entry
    //     for that file is unspecified.
    if cookie == wasi::__WASI_DIRCOOKIE_START {
        trace!("     | fd_readdir: doing rewinddir");
        dir.rewind();
    } else {
        trace!("     | fd_readdir: doing seekdir to {}", cookie);
        let loc = unsafe { SeekLoc::from_raw(cookie as i64) };
        dir.seek(loc);
    }

    Ok(DirIter(dir).map(|entry| {
        let entry: Entry = entry?;
        Ok(Dirent {
            name: entry // TODO can we reuse path_from_host for CStr?
                .file_name()
                .to_str()?
                .to_owned(),
            ino: entry.ino(),
            ftype: entry.file_type().into(),
            cookie: entry.seek_loc().to_raw().try_into()?,
        })
    }))
}

struct DirIter(Dir);

impl Iterator for DirIter {
    type Item = nix::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        use libc::{dirent64, readdir64_r};
        use nix::errno::Errno;

        unsafe {
            // Note: POSIX specifies that portable applications should dynamically allocate a
            // buffer with room for a `d_name` field of size `pathconf(..., _PC_NAME_MAX)` plus 1
            // for the NUL byte. It doesn't look like the std library does this; it just uses
            // fixed-sized buffers (and libc's dirent seems to be sized so this is appropriate).
            // Probably fine here too then.
            //
            // See `impl Iterator for ReadDir` [1] for more details.
            // [1] https://github.com/rust-lang/rust/blob/master/src/libstd/sys/unix/fs.rs
            let mut ent = std::mem::MaybeUninit::<dirent64>::uninit();
            let mut result = std::ptr::null_mut();
            if let Err(e) = Errno::result(readdir64_r(
                (self.0).0.as_ptr(),
                ent.as_mut_ptr(),
                &mut result,
            )) {
                return Some(Err(e));
            }
            if result.is_null() {
                None
            } else {
                assert_eq!(result, ent.as_mut_ptr(), "readdir_r specification violated");
                Some(Ok(Entry(ent.assume_init())))
            }
        }
    }
}

pub(crate) fn fd_advise(
    file: &File,
    advice: wasi::__wasi_advice_t,
    offset: wasi::__wasi_filesize_t,
    len: wasi::__wasi_filesize_t,
) -> Result<()> {
    {
        use nix::fcntl::{posix_fadvise, PosixFadviseAdvice};

        let offset = offset.try_into()?;
        let len = len.try_into()?;
        let host_advice = match advice {
            wasi::__WASI_ADVICE_DONTNEED => PosixFadviseAdvice::POSIX_FADV_DONTNEED,
            wasi::__WASI_ADVICE_SEQUENTIAL => PosixFadviseAdvice::POSIX_FADV_SEQUENTIAL,
            wasi::__WASI_ADVICE_WILLNEED => PosixFadviseAdvice::POSIX_FADV_WILLNEED,
            wasi::__WASI_ADVICE_NOREUSE => PosixFadviseAdvice::POSIX_FADV_NOREUSE,
            wasi::__WASI_ADVICE_RANDOM => PosixFadviseAdvice::POSIX_FADV_RANDOM,
            wasi::__WASI_ADVICE_NORMAL => PosixFadviseAdvice::POSIX_FADV_NORMAL,
            _ => return Err(Error::EINVAL),
        };

        posix_fadvise(file.as_raw_fd(), offset, len, host_advice)?;
    }

    Ok(())
}
