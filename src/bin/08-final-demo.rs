use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nix::errno::Errno;
use nix::fcntl::{self, FcntlArg, OFlag};
use nix::sys::select::{self, FdSet};
use nix::sys::socket::{self, AddressFamily, MsgFlags, SockFlag, SockType, SockaddrIn};
use nix::unistd;

thread_local! {
    static NEW_TASKS: RefCell<Vec<Pin<Box<dyn Future<Output = ()>>>>> = RefCell::new(Vec::new());

    static SELECTOR_PING_PIPE: RefCell<(OwnedFd, OwnedFd)> = {
        let (rx, tx) = unistd::pipe().unwrap();
        set_non_blocking(rx.as_fd());
        set_non_blocking(tx.as_fd());

        RefCell::new((rx, tx))
    };

    static SENT_BYTES: RefCell<usize> = const { RefCell::new(0) };
}

static NEW_WFDS: LazyLock<Mutex<HashMap<i32, Waker>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn set_non_blocking(fd: BorrowedFd) {
    let flags = OFlag::from_bits_truncate(fcntl::fcntl(fd, FcntlArg::F_GETFL).unwrap());
    fcntl::fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
}

fn spawn<F: Future<Output = ()> + 'static>(fut: F) {
    NEW_TASKS.with_borrow_mut(|futs| {
        futs.push(Box::pin(fut));
    });
}

fn register_wfd(fd: i32, waker: Waker) {
    NEW_WFDS.lock().unwrap().insert(fd, waker);

    SELECTOR_PING_PIPE.with_borrow(|(_rx, tx)| {
        unistd::write(tx, &[0]).unwrap();
    });
}

struct ThreadWaker {
    thread: thread::Thread,
    task_id: usize,
    task_ids_to_poll: Arc<Mutex<HashSet<usize>>>,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.task_ids_to_poll.lock().unwrap().insert(self.task_id);
        self.thread.unpark();
    }
}

struct Sleep {
    duration: Duration,
    handle: Option<JoinHandle<()>>,
    ready: Arc<Mutex<bool>>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if *self.ready.lock().unwrap() {
            Poll::Ready(())
        } else {
            if self.handle.is_none() {
                let duration = self.duration;
                let ready = Arc::clone(&self.ready);
                let waker = cx.waker().clone();

                self.handle = Some(thread::spawn(move || {
                    thread::sleep(duration);
                    *ready.lock().unwrap() = true;
                    waker.wake();
                }));
            }

            Poll::Pending
        }
    }
}

#[must_use]
fn sleep(duration: Duration) -> Sleep {
    Sleep {
        duration,
        handle: None,
        ready: Arc::new(Mutex::new(false)),
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
            Poll::Ready(TcpStream(self.sock.take().unwrap()))
        } else {
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

            Err(_) => {
                if !self.registered {
                    register_wfd(fd, cx.waker().clone());
                }

                Poll::Pending
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

struct Task {
    fut: Pin<Box<dyn Future<Output = ()>>>,
    waker: Waker,
}

async fn tcp_client(addr: &str) {
    let mut stream = TcpStream::connect(addr).await;
    let data: Vec<u8> = vec![42; 1024 * 1024];

    loop {
        let mut buf = &data[..];

        while !buf.is_empty() {
            let n = stream.write(buf).await;
            buf = &buf[n..];
            SENT_BYTES.with_borrow_mut(|bytes| *bytes += n);
        }
    }
}

async fn spinner() {
    let symbols = b"-\\|/";
    let mut i = 0;
    let mut stdout = io::stdout();

    loop {
        let symbol = symbols[i] as char;
        i = (i + 1) % symbols.len();
        let sent = SENT_BYTES.with_borrow(|bytes| *bytes);
        let _ = write!(stdout, "\r{symbol} {sent}");
        let _ = stdout.flush();
        sleep(Duration::from_millis(100)).await;
    }
}

fn main() {
    let mut tasks: HashMap<usize, Task> = HashMap::new();
    let mut next_task_id: usize = 0;
    let task_ids_to_poll: Arc<Mutex<HashSet<usize>>> = Arc::new(Mutex::new(HashSet::new()));

    let ping_rx = SELECTOR_PING_PIPE.with_borrow(|(rx, _tx)| rx.as_raw_fd());
    let selector_handle = thread::spawn(move || selector(ping_rx));

    spawn(tcp_client("127.0.0.1:3333"));
    spawn(spinner());

    loop {
        NEW_TASKS.with_borrow_mut(|futs| {
            for mut fut in futs.drain(..) {
                let task_id = next_task_id;

                let waker = Arc::new(ThreadWaker {
                    thread: thread::current(),
                    task_id,
                    task_ids_to_poll: Arc::clone(&task_ids_to_poll),
                })
                .into();

                next_task_id += 1;
                let mut ctx = Context::from_waker(&waker);

                if fut.as_mut().poll(&mut ctx).is_pending() {
                    tasks.insert(task_id, Task { fut, waker });
                }
            }
        });

        if tasks.is_empty() { break; }

        std::thread::park();

        for id in task_ids_to_poll.lock().unwrap().drain() {
            let task = tasks.get_mut(&id).unwrap();
            let waker = task.waker.clone();
            let mut ctx = Context::from_waker(&waker);

            if task.fut.as_mut().poll(&mut ctx).is_ready() {
                tasks.remove(&id);
            }
        }
    }

    println!("all tasks finished");

    selector_handle.join().unwrap();
}
