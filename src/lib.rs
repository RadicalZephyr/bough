use std::marker::PhantomData;

pub struct Transaction; // opaque

pub struct Stream<T> {
    // initially: just a handle into a single-threaded runtime
    _phantom: PhantomData<T>,
}

impl<T> Stream<T> {
    pub fn listen(&self, f: impl FnMut(T)) -> impl FnOnce() {
        || todo!()
    }

    pub fn sync_receiver(&self, bound: usize) -> StreamSyncReceiver<T> {
        StreamSyncReceiver::new()
    }

    pub fn receiver(&self) -> StreamReceiver<T> {
        StreamReceiver::new()
    }
}

pub struct StreamSyncReceiver<T> {
    _phantom: PhantomData<T>,
}

impl<T> StreamSyncReceiver<T> {
    fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    // TODO [ZEFS 2025-11-22 Github#3]: Should this be fallible?
    pub fn recv(&self) -> Result<T, RecvError> {
        todo!()
    }
}

pub struct StreamReceiver<T> {
    _phantom: PhantomData<T>,
}

impl<T> StreamReceiver<T> {
    fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecvError;

pub struct StreamSender<T> {
    _phantom: PhantomData<T>,
}

impl<T> StreamSender<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    pub fn stream(&self) -> Stream<T> {
        Stream {
            _phantom: PhantomData,
        }
    }

    pub fn send(&self, t: T) {}
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    #[test]
    fn listen_interface() {
        let tx = StreamSender::<u8>::new();

        let s = tx.stream();

        let observed: Arc<Mutex<Vec<u8>>> = Arc::default();
        let _unsub = s.listen(|x| observed.lock().unwrap().push(x));

        tx.send(42);

        assert_eq!(*observed.lock().unwrap(), vec![42]);
    }

    #[test]
    fn sync_channel_interface() {
        let tx = StreamSender::<u8>::new();

        let s = tx.stream();

        thread::scope(|sc| {
            sc.spawn(|| {
                let rx = s.sync_receiver(2);
                assert_eq!(42, rx.recv().expect("unexpected receive error"));
            });
            tx.send(42);
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn async_channel_interface() {
        let tx = StreamSender::<u8>::new();

        let s = tx.stream();

        let rx = s.receiver();
    }
}
