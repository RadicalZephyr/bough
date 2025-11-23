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

    pub fn receiver(&self) -> StreamReceiver<T> {
        StreamReceiver::new()
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

    pub fn recv(&self) -> Result<T, RecvError> {
        todo!()
    }
}

pub struct RecvError;

pub struct StreamSink<T> {
    _phantom: PhantomData<T>,
}

impl<T> StreamSink<T> {
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
        let sink = StreamSink::<u8>::new();

        let s = sink.stream();

        let observed: Arc<Mutex<Vec<u8>>> = Arc::default();
        let _unsub = s.listen(|x| observed.lock().unwrap().push(x));

        sink.send(42);

        assert_eq!(*observed.lock().unwrap(), vec![42]);
    }

    #[test]
    fn sync_channel_interface() {
        let sink = StreamSink::<u8>::new();

        let s = sink.stream();

        thread::scope(|sc| {
            sc.spawn(|| {
                let rec = s.receiver();
            });
            sink.send(42);
        });
    }
}
