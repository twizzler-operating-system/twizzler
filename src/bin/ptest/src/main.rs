use std::fmt::Debug;

use naming::GetFlags;
use twizzler::{
    Invariant,
    alloc::{
        arena::{ArenaAllocator, ArenaObject},
        invbox::InvBox,
    },
    collections::vec::{VecObject, VecObjectAlloc},
    object::{MapFlags, Object, ObjectBuilder},
};

#[derive(Invariant)]
struct Foo {
    data: InvBox<u32, ArenaAllocator>,
    local_data: u32,
}

impl Debug for Foo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Foo")
            .field("local_data", &self.local_data)
            .field("data (ptr)", &self.data.global())
            .field("data (val)", &*self.data.resolve())
            .finish()
    }
}

fn main() {
    let mut nh = naming::static_naming_factory().unwrap();
    let mut vo = if let Ok(node) = nh.get("/data/ptest-obj", GetFlags::empty()) {
        println!("reopened: {:?}", node.id);
        VecObject::from(
            Object::map(
                node.id,
                MapFlags::PERSIST | MapFlags::READ | MapFlags::WRITE,
            )
            .unwrap(),
        )
    } else {
        let obj = ObjectBuilder::default().persist();
        let vo = VecObject::<Foo, VecObjectAlloc>::new(obj).unwrap();

        println!("new: {:?}", vo.object().id());
        nh.put("/data/ptest-obj", vo.object().id()).unwrap();
        vo
    };
    for e in &vo {
        println!("current contents: {:?}", e);
    }
    let len = vo.iter().count();
    println!("pushing items");
    let start = std::time::Instant::now();
    let alloc = ArenaObject::new(ObjectBuilder::default().persist()).unwrap();
    for i in 0..3 {
        //println!("pushing: {}", i);
        //vo.push(i).unwrap();
        vo.push_ctor(|tx| {
            let foo = Foo {
                local_data: i + len as u32,
                data: InvBox::new_in(&tx, i, alloc.allocator()).unwrap(),
            };

            tx.write(foo)
        })
        .unwrap();
    }
    let end = std::time::Instant::now();
    println!("done!: {:?}", end - start);
    /*
    for e in &vo {
        println!("current contents: {:?}", e);
    }
    */
}
