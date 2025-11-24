use std::marker::PhantomData;

struct Context {}

impl Context {
    fn new() -> Self {
        Self {}
    }

    fn build<'build>(&'build mut self) -> BuildCtx<'build> {
        BuildCtx { context: self }
    }

    fn io<'io>(&'io mut self) -> IoCtx<'io> {
        IoCtx { context: self }
    }
}

struct BuildCtx<'build> {
    context: &'build mut Context,
}

pub struct IoCtx<'io> {
    context: &'io mut Context,
}

impl<'io> IoCtx<'io> {
    fn send<Event>(&self, i: &StreamSender<Event>, event: Event) {
        todo!()
    }
}

pub trait BuildInputs: Clone + Sized {
    type Edge;
    fn build(ctx: &mut BuildCtx) -> (Self, Self::Edge);
}

pub trait BuildOutputs {
    type Edge;
    fn edge_view(&self, ctx: &mut BuildCtx) -> Self::Edge;
}

pub struct Runtime<In: Sized + BuildInputs, Out: Sized + BuildOutputs> {
    graph_inputs: In,
    edge_inputs: <In as BuildInputs>::Edge,
    graph_outputs: Out,
    edge_outputs: <Out as BuildOutputs>::Edge,
}

impl<I: Sized + BuildInputs, O: Sized + BuildOutputs> Runtime<I, O> {
    pub fn build<F>(f: F) -> Self
    where
        F: for<'b> FnOnce(I) -> O,
    {
        let mut context = Context::new();
        let mut build_ctx: BuildCtx<'_> = context.build();
        let (graph_inputs, edge_inputs) = <I as BuildInputs>::build(&mut build_ctx);
        let graph_outputs = f(graph_inputs.clone());
        let edge_outputs = graph_outputs.edge_view(&mut build_ctx);

        Self {
            graph_inputs,
            edge_inputs,
            graph_outputs,
            edge_outputs,
        }
    }

    pub fn with_io<R>(
        &self,
        f: impl for<'io> FnOnce(IoCtx<'io>, &<I as BuildInputs>::Edge, &<O as BuildOutputs>::Edge) -> R,
    ) -> R {
        let mut context = Context::new();
        let io_ctx = context.io();
        f(io_ctx, &self.edge_inputs, &self.edge_outputs)
    }
}

pub struct StreamSender<Event> {
    _p: PhantomData<Event>,
}

pub struct Stream<Event> {
    _p: PhantomData<Event>,
}

impl<Event> Clone for Stream<Event> {
    fn clone(&self) -> Self {
        Self {
            _p: self._p.clone(),
        }
    }
}

pub struct StreamReceiver<Event> {
    _p: PhantomData<Event>,
}

impl<Event> BuildInputs for Stream<Event> {
    type Edge = StreamSender<Event>;

    fn build(ctx: &mut BuildCtx) -> (Self, Self::Edge) {
        todo!()
    }
}

impl<Event> BuildOutputs for Stream<Event> {
    type Edge = StreamReceiver<Event>;

    fn edge_view(&self, ctx: &mut BuildCtx) -> Self::Edge {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let rt = Runtime::build(|inputs: Stream<usize>| inputs);

        rt.with_io(|ctx, input, output| {
            ctx.send(input, 0);

            assert_eq!(0, ctx.recv(output));
        });
    }
}
