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
}

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
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn it_works() {
        let sink = StreamSink::<u8>::new();

        let s = sink.stream();

        let observed: Arc<Mutex<Vec<u8>>> = Arc::default();
        let _unsub = s.listen(|x| observed.lock().unwrap().push(x));

        sink.send(42);

        assert_eq!(*observed.lock().unwrap(), vec![42]);
    }
}
