// https://is.gd/rustikon

use std::task::{Context, Poll, Waker};

async fn printer() {
    println!("hello!");
}

fn main() {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let fut = printer();
    let mut fut = Box::pin(fut);

    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(_) => {
            println!("ready");
        }

        Poll::Pending => {
            println!("pending");
        }
    }
}
