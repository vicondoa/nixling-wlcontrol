use std::{
    io,
    os::fd::{AsFd as _, AsRawFd as _, OwnedFd},
    path::Path,
    time::Duration,
};

use nix::{
    errno::Errno,
    poll::{poll, PollFd, PollFlags, PollTimeout},
    sys::socket::{
        connect, getsockopt, recv, send, socket, sockopt, AddressFamily, MsgFlags, SockFlag,
        SockType, UnixAddr,
    },
};
use wlcontrol_core::error::{WlError, WlResult};

use crate::wire::{decode_frame, encode_frame, MAX_FRAME_BYTES};

#[derive(Debug)]
pub(crate) struct SeqpacketTransport {
    fd: OwnedFd,
    timeout: Duration,
}

impl SeqpacketTransport {
    pub(crate) fn connect(path: &Path, timeout: Duration) -> WlResult<Self> {
        if !path.exists() {
            return Err(WlError::DaemonDown(format!(
                "{} does not exist",
                path.display()
            )));
        }

        let fd = socket(
            AddressFamily::Unix,
            SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
            None,
        )
        .map_err(errno_to_io)?;
        let addr = UnixAddr::new(path).map_err(|err| {
            WlError::Config(format!(
                "invalid public socket path {}: {err}",
                path.display()
            ))
        })?;

        match connect(fd.as_raw_fd(), &addr) {
            Ok(()) => Ok(Self { fd, timeout }),
            Err(Errno::EINPROGRESS) | Err(Errno::EALREADY) => {
                wait_for(&fd, PollFlags::POLLOUT, timeout, "connect").map_err(|err| {
                    if let WlError::Timeout(message) = err {
                        WlError::DaemonDown(message)
                    } else {
                        err
                    }
                })?;
                let socket_error = getsockopt(&fd, sockopt::SocketError).map_err(errno_to_io)?;
                if socket_error == 0 {
                    Ok(Self { fd, timeout })
                } else {
                    Err(connect_io_error(
                        path,
                        io::Error::from_raw_os_error(socket_error),
                    ))
                }
            }
            Err(Errno::EISCONN) => Ok(Self { fd, timeout }),
            Err(err) => Err(connect_io_error(path, errno_to_io(err))),
        }
    }

    pub(crate) fn send_payload(&self, payload: &[u8]) -> WlResult<()> {
        let frame = encode_frame(payload)?;
        wait_for(&self.fd, PollFlags::POLLOUT, self.timeout, "send")?;
        let sent = send(self.fd.as_raw_fd(), &frame, MsgFlags::MSG_DONTWAIT).map_err(|err| {
            if matches!(err, Errno::EAGAIN) {
                WlError::Timeout(format!("send to d2bd after {:?}", self.timeout))
            } else if matches!(err, Errno::EPIPE | Errno::ECONNRESET | Errno::ENOTCONN) {
                WlError::DaemonDown(format!("d2bd public socket closed during send: {err}"))
            } else {
                WlError::Io(errno_to_io(err))
            }
        })?;
        if sent == frame.len() {
            Ok(())
        } else {
            Err(WlError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write on seqpacket socket",
            )))
        }
    }

    pub(crate) fn recv_payload(&self) -> WlResult<Vec<u8>> {
        wait_for(&self.fd, PollFlags::POLLIN, self.timeout, "receive")?;
        let mut buffer = vec![0_u8; MAX_FRAME_BYTES + 4];
        let received =
            recv(self.fd.as_raw_fd(), &mut buffer, MsgFlags::MSG_DONTWAIT).map_err(|err| {
                if matches!(err, Errno::EAGAIN) {
                    WlError::Timeout(format!("receive from d2bd after {:?}", self.timeout))
                } else {
                    WlError::Io(errno_to_io(err))
                }
            })?;
        if received == 0 {
            return Err(WlError::DaemonDown(
                "d2bd public socket closed during receive".to_owned(),
            ));
        }
        let payload = decode_frame(&buffer[..received])?;
        Ok(payload.to_vec())
    }
}

fn wait_for(fd: &OwnedFd, events: PollFlags, timeout: Duration, op: &str) -> WlResult<()> {
    let mut pollfd = [PollFd::new(fd.as_fd(), events)];
    let timeout = PollTimeout::try_from(timeout)
        .map_err(|err| WlError::Config(format!("invalid d2bd timeout: {err}")))?;
    let ready = poll(&mut pollfd, timeout).map_err(errno_to_io)?;
    if ready == 0 {
        return Err(WlError::Timeout(format!(
            "{op} on d2bd public socket after {:?}",
            timeout.duration().unwrap_or_default()
        )));
    }
    let revents = pollfd[0].revents().unwrap_or_else(PollFlags::empty);
    if revents.intersects(PollFlags::POLLERR | PollFlags::POLLNVAL)
        || (revents.contains(PollFlags::POLLHUP) && !revents.intersects(events))
    {
        return Err(WlError::DaemonDown(format!(
            "{op} on d2bd public socket failed ({revents:?})"
        )));
    }
    Ok(())
}

fn connect_io_error(path: &Path, err: io::Error) -> WlError {
    match err.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::TimedOut => {
            WlError::DaemonDown(format!("failed to connect to {}: {err}", path.display()))
        }
        io::ErrorKind::PermissionDenied => WlError::Denied(format!(
            "permission denied connecting to {}: {err}",
            path.display()
        )),
        _ => WlError::Io(err),
    }
}

fn errno_to_io(errno: Errno) -> io::Error {
    io::Error::from_raw_os_error(errno as i32)
}
