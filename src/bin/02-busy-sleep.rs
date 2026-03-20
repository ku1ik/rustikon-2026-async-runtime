use std::io::Write;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::{io, thread};
use std::time::{Duration, Instant};

struct Sleep {
    duration: Duration,
    since: Instant,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.since.elapsed() >= self.duration {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[must_use]
fn sleep(duration: Duration) -> Sleep {
    Sleep {
        duration,
        since: Instant::now(),
    }
}

async fn printer() {
    println!("printer: going to sleep");
    sleep(Duration::from_secs(1)).await;
    println!("printer: had a nice nap");
    sleep(Duration::from_secs(2)).await;
    println!("printer: had another nice nap");
}

fn main() {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let fut = printer();
    let mut fut = Box::pin(fut);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {
                println!("ready");
                break;
            }

            Poll::Pending => {
                print!(".");
                let _ = io::stdout().flush();
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
