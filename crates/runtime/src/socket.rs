//! TCP sockets (SPECS §6 I/O: the Redtamarin-shaped CLI surface, not
//! `flash.net`). Blocking, synchronous, strings in/out as UTF-8 — a
//! byte-buffer type can layer under this later.
//!
//! One runtime object backs both language types: `Socket` (a connected
//! stream, buffered for `readLine`) and `ServerSocket` (a listener).
//! Sema's nominal types keep the method sets apart; the runtime enum is
//! just storage. Dead sockets close on GC sweep (drop closes the fd),
//! but programs should call `close()` — collection time is not
//! deterministic.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::exc;
use crate::gc;
use crate::string::VsString;

/// Socket payload: a buffered stream or a listener. `Closed` after
/// `close()` — later operations throw a catchable Error.
pub enum Kind {
    /// Connected stream. Reads go through the BufReader (readLine);
    /// writes through the cloned handle.
    Stream {
        /// Buffered read half.
        reader: BufReader<TcpStream>,
        /// Write half (`try_clone` of the same fd).
        writer: TcpStream,
    },
    /// Listening server socket.
    Listener(TcpListener),
    /// Explicitly closed.
    Closed,
}

/// The socket runtime object (GC kind Socket: dropped on sweep, which
/// closes the descriptor).
pub struct VsSocket {
    /// Current state.
    pub kind: std::cell::RefCell<Kind>,
}

fn alloc(kind: Kind) -> *const VsSocket {
    let p = gc::alloc(std::mem::size_of::<VsSocket>(), gc::Kind::Socket) as *mut VsSocket;
    // SAFETY: fresh block of exactly VsSocket size.
    unsafe {
        p.write(VsSocket {
            kind: std::cell::RefCell::new(kind),
        })
    };
    p
}

fn io_error(op: &str, e: std::io::Error) -> ! {
    exc::throw_error(exc::ErrorKind::Error, &format!("{op}: {e}"))
}

/// `Socket.connect(host, port)` — blocking TCP connect.
pub fn connect(host: &str, port: u16) -> *const VsSocket {
    let stream = match TcpStream::connect((host, port)) {
        Ok(s) => s,
        Err(e) => io_error(&format!("Socket.connect({host}:{port})"), e),
    };
    stream_socket(stream)
}

fn stream_socket(stream: TcpStream) -> *const VsSocket {
    let writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => io_error("Socket.connect", e),
    };
    alloc(Kind::Stream {
        reader: BufReader::new(stream),
        writer,
    })
}

/// `ServerSocket.bind(port)` — listen on all interfaces.
pub fn bind(port: u16) -> *const VsSocket {
    match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => alloc(Kind::Listener(l)),
        Err(e) => io_error(&format!("ServerSocket.bind({port})"), e),
    }
}

/// `server.accept()` — blocking accept, returns a connected Socket.
pub fn accept(sock: &VsSocket) -> *const VsSocket {
    let kind = sock.kind.borrow();
    match &*kind {
        Kind::Listener(l) => match l.accept() {
            Ok((stream, _)) => {
                drop(kind);
                stream_socket(stream)
            }
            Err(e) => io_error("accept()", e),
        },
        _ => exc::throw_error(exc::ErrorKind::Type, "accept() needs a ServerSocket"),
    }
}

/// `socket.write(data)` — sends the UTF-8 bytes of `data`.
pub fn write(sock: &VsSocket, data: &VsString) {
    let mut kind = sock.kind.borrow_mut();
    match &mut *kind {
        Kind::Stream { writer, .. } => {
            if let Err(e) = writer.write_all(data.to_rust().as_bytes()) {
                io_error("write()", e);
            }
        }
        Kind::Closed => exc::throw_error(exc::ErrorKind::Error, "write() on a closed socket"),
        Kind::Listener(_) => {
            exc::throw_error(exc::ErrorKind::Type, "write() needs a connected Socket")
        }
    }
}

/// `socket.readLine()` — next line without its terminator (`\n` or
/// `\r\n`); null at EOF.
pub fn read_line(sock: &VsSocket) -> *const VsString {
    let mut kind = sock.kind.borrow_mut();
    match &mut *kind {
        Kind::Stream { reader, .. } => {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => std::ptr::null(),
                Ok(_) => {
                    while line.ends_with('\n') || line.ends_with('\r') {
                        line.pop();
                    }
                    VsString::from_rust(&line)
                }
                Err(e) => io_error("readLine()", e),
            }
        }
        Kind::Closed => exc::throw_error(exc::ErrorKind::Error, "readLine() on a closed socket"),
        Kind::Listener(_) => {
            exc::throw_error(exc::ErrorKind::Type, "readLine() needs a connected Socket")
        }
    }
}

/// `socket.read(max)` — up to `max` bytes as a UTF-8 string (lossy);
/// null at EOF.
pub fn read(sock: &VsSocket, max: usize) -> *const VsString {
    let mut kind = sock.kind.borrow_mut();
    match &mut *kind {
        Kind::Stream { reader, .. } => {
            let mut buf = vec![0u8; max.clamp(1, 1 << 20)];
            match reader.read(&mut buf) {
                Ok(0) => std::ptr::null(),
                Ok(n) => VsString::from_rust(&String::from_utf8_lossy(&buf[..n])),
                Err(e) => io_error("read()", e),
            }
        }
        Kind::Closed => exc::throw_error(exc::ErrorKind::Error, "read() on a closed socket"),
        Kind::Listener(_) => {
            exc::throw_error(exc::ErrorKind::Type, "read() needs a connected Socket")
        }
    }
}

/// `close()` — drops the descriptor; further use throws.
pub fn close(sock: &VsSocket) {
    *sock.kind.borrow_mut() = Kind::Closed;
}

/// The listener's bound port (`server.localPort`) — useful with bind(0).
pub fn local_port(sock: &VsSocket) -> i32 {
    let kind = sock.kind.borrow();
    match &*kind {
        Kind::Listener(l) => l.local_addr().map(|a| i32::from(a.port())).unwrap_or(-1),
        Kind::Stream { writer, .. } => writer
            .local_addr()
            .map(|a| i32::from(a.port()))
            .unwrap_or(-1),
        Kind::Closed => -1,
    }
}
