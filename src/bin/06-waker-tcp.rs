use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use nix::errno::Errno;
use nix::fcntl::{self, FcntlArg, OFlag};
use nix::sys::select::{self, FdSet};
use nix::sys::socket::{self, AddressFamily, MsgFlags, SockFlag, SockType, SockaddrIn};
use nix::unistd;

thread_local! {
    static SELECTOR_PING_PIPE: RefCell<(OwnedFd, OwnedFd)> = {
        let (rx, tx) = unistd::pipe().unwrap();
        set_non_blocking(rx.as_fd());
        set_non_blocking(tx.as_fd());

        RefCell::new((rx, tx))
    };
}

static NEW_WFDS: LazyLock<Mutex<HashMap<i32, Waker>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn set_non_blocking(fd: BorrowedFd) {
    let flags = OFlag::from_bits_truncate(fcntl::fcntl(fd, FcntlArg::F_GETFL).unwrap());
    fcntl::fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        println!("unparking");
        self.0.unpark();
    }
}

struct TcpStream(OwnedFd);

impl TcpStream {
    fn connect(addr: &str) -> TcpStreamConnect {
        let sock = socket::socket(AddressFamily::Inet, SockType::Stream, SockFlag::empty(), None).unwrap();
        set_non_blocking(sock.as_fd());

        TcpStreamConnect {
            sock: Some(sock),
            addr: SockaddrIn::from_str(addr).unwrap(),
            connecting: false,
        }
    }

    fn write<'a>(&'a mut self, data: &'a [u8]) -> TcpStreamWrite<'a> {
        TcpStreamWrite {
            sock: self.0.as_fd(),
            data,
            registered: false,
        }
    }
}

struct TcpStreamConnect {
    sock: Option<OwnedFd>,
    addr: SockaddrIn,
    connecting: bool,
}

impl Future for TcpStreamConnect {
    type Output = TcpStream;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.connecting {
            println!("tcp stream: connected");

            Poll::Ready(TcpStream(self.sock.take().unwrap()))
        } else {
            println!("tcp stream: connecting...");
            let fd = self.sock.as_ref().unwrap().as_raw_fd();
            register_wfd(fd, cx.waker().clone());
            let _ = socket::connect(fd, &self.addr);
            self.connecting = true;

            Poll::Pending
        }
    }
}

struct TcpStreamWrite<'a> {
    sock: BorrowedFd<'a>,
    data: &'a [u8],
    registered: bool,
}

impl<'a> Future for TcpStreamWrite<'a> {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fd = self.sock.as_raw_fd();

        match socket::send(fd, self.data, MsgFlags::empty()) {
            Ok(n) => Poll::Ready(n),

            Err(_) => { // WouldBlock / EAGAIN
                println!("tcp stream: not ready for send...");

                if !self.registered {
                    register_wfd(fd, cx.waker().clone());
                }

                Poll::Pending
            }
        }
    }
}

async fn tcp_client(addr: &str) {
    let mut stream = TcpStream::connect(addr).await;

    for _ in 0..10 {
        let n = stream.write("hello\r\n".as_bytes()).await;
        println!("wrote {n} bytes to the socket");
    }
}

fn selector(ping_rx: i32) {
    let ping_rx = unsafe { BorrowedFd::borrow_raw(ping_rx) };
    let mut writers: HashMap<i32, Waker> = HashMap::new();

    loop {
        let mut r_fd_set = FdSet::new();
        let mut w_fd_set = FdSet::new();

        r_fd_set.insert(ping_rx);

        for fd in writers.keys() {
            w_fd_set.insert(unsafe { BorrowedFd::borrow_raw(*fd) });
        }

        select::select(None, &mut r_fd_set, &mut w_fd_set, None, None).unwrap();

        writers.retain(|fd, waker| {
            let fd = unsafe { BorrowedFd::borrow_raw(*fd) };

            if w_fd_set.contains(fd) {
                waker.wake_by_ref();
                false
            } else {
                true
            }
        });

        if r_fd_set.contains(ping_rx) {
            drain_fd(ping_rx);

            for (fd, waker) in NEW_WFDS.lock().unwrap().drain() {
                writers.insert(fd, waker);
            }
        }
    }
}

fn drain_fd(fd: BorrowedFd) {
    let mut buf = [0; 16];

    loop {
        match unistd::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(Errno::EAGAIN) => break,
            Err(e) => panic!("{e}"),
        }
    }
}

fn register_wfd(fd: i32, waker: Waker) {
    NEW_WFDS.lock().unwrap().insert(fd, waker);

    SELECTOR_PING_PIPE.with_borrow(|(_rx, tx)| {
        unistd::write(tx, &[0]).unwrap();
    });
}

fn main() {
    let waker = Arc::new(ThreadWaker(thread::current())).into();
    let mut cx = Context::from_waker(&waker);

    let ping_rx = SELECTOR_PING_PIPE.with_borrow(|(rx, _tx)| rx.as_raw_fd());
    let selector_handle = thread::spawn(move || selector(ping_rx));

    let fut = tcp_client("127.0.0.1:3333");
    let mut fut = Box::pin(fut);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {
                println!("ready");
                break;
            }

            Poll::Pending => {}
        }

        println!("parking");
        thread::park();
    }

    println!("task finished");

    selector_handle.join().unwrap();
}
