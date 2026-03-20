use std::cell::RefCell;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

thread_local! {
    static NEW_TASKS: RefCell<Vec<Pin<Box<dyn Future<Output = ()>>>>> = RefCell::new(Vec::new());
}

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

fn spawn<F: Future<Output = ()> + 'static>(fut: F) {
    NEW_TASKS.with_borrow_mut(|futs| {
        futs.push(Box::pin(fut));
    })
}

async fn printer() {
    spawn(sleeper());
    println!("printer: going to sleep");
    sleep(Duration::from_secs(1)).await;
    println!("printer: had a nice nap");
    sleep(Duration::from_secs(2)).await;
    println!("printer: had another nice nap");
}

async fn sleeper() {
    sleep(Duration::from_secs(3)).await;
    println!("sleeper: woke up");
}

fn main() {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut tasks: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();

    spawn(printer());

    loop {
        NEW_TASKS.with_borrow_mut(|futs| { tasks.append(futs); });

        if tasks.is_empty() { break; }

        tasks.retain_mut(|fut| match fut.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {
                println!("ready");
                false
            }

            Poll::Pending => true,
        });

        thread::sleep(Duration::from_millis(1));
    }

    println!("all done!");
}
