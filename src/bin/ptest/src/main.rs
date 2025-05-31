use naming::GetFlags;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{Object, ObjectBuilder},
};
use twizzler_rt_abi::object::MapFlags;

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
        let vo = VecObject::<u64, VecObjectAlloc>::new(obj).unwrap();

        println!("new: {:?}", vo.object().id());
        nh.put("/data/ptest-obj", vo.object().id()).unwrap();
        vo
    };
    for e in &vo {
        println!("current contents: {:?}", e);
    }
    println!("pushing!");
    let start = std::time::Instant::now();
    for i in 0..1000 {
        vo.push(64).unwrap();
    }
    let end = std::time::Instant::now();
    println!("done!: {:?}", end - start);
    for e in &vo {
        println!("current contents: {:?}", e);
    }
}
