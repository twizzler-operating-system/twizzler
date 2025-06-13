use std::marker::PhantomData;

use super::{Result, TxObject};
use crate::object::Object;

#[derive(Default)]
#[allow(dead_code)]
pub struct TxBatch {
    txs: Vec<Box<TxObject<()>>>,
}

#[repr(transparent)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HandleIdx<B>(usize, PhantomData<B>);

impl<B> Clone for HandleIdx<B> {
    fn clone(&self) -> Self {
        Self(self.0, self.1)
    }
}

impl<B> Copy for HandleIdx<B> {}

#[allow(unused_variables)]
impl TxBatch {
    pub fn tx<B>(&mut self, obj: Object<B>) -> Result<HandleIdx<B>> {
        todo!()
    }

    pub fn handle<B>(&self, idx: HandleIdx<B>) -> &TxObject<B> {
        todo!()
    }

    pub fn handle_mut<B>(&mut self, idx: HandleIdx<B>) -> &mut TxObject<B> {
        todo!()
    }

    pub fn commit<B>(&mut self, idx: HandleIdx<B>) -> Result<Object<B>> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
        tx::TxBatch,
    };

    #[allow(dead_code)]
    struct Simple {
        x: u32,
    }

    impl BaseType for Simple {}

    //#[test]
    #[allow(dead_code)]
    fn simple_batch_tx() {
        let builder = ObjectBuilder::default();
        let obj1 = builder.build(Simple { x: 3 }).unwrap();
        let obj2 = builder.build(Simple { x: 7 }).unwrap();

        let (obj1, obj2) = {
            let mut batch = TxBatch::default();
            let tx1 = batch.tx(obj1).unwrap();
            let tx2 = batch.tx(obj2).unwrap();
            batch.handle_mut(tx1).base_mut().x = 8;
            batch.handle_mut(tx2).base_mut().x = 12;
            let obj1 = batch.commit(tx1).unwrap();
            let obj2 = batch.commit(tx2).unwrap();
            (obj1, obj2)
        };

        assert_eq!(obj1.base().x, 8);
        assert_eq!(obj2.base().x, 12);
    }
}
