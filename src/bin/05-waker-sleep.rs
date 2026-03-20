use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

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

async fn printer() {
    println!("printer: going to sleep");
    sleep(Duration::from_secs(3)).await;
    println!("printer: had a nice nap");
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        println!("unparking");
        self.0.unpark();
    }
}

fn main() {
    let waker = Arc::new(ThreadWaker(thread::current())).into();
    let mut cx = Context::from_waker(&waker);

    let fut = printer();
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
}
