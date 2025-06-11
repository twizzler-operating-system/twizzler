use std::fmt::Debug;

use clap::Parser;
use miette::{IntoDiagnostic, Result};
use naming::{GetFlags, StaticNamingAPI};
use twizzler::{
    Invariant,
    alloc::{
        arena::{ArenaAllocator, ArenaObject},
        invbox::InvBox,
    },
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder},
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

fn create_vector_object<T: Debug + Invariant>() -> Result<VecObject<T, VecObjectAlloc>> {
    let obj = ObjectBuilder::default().persist();
    VecObject::<T, VecObjectAlloc>::new(obj).into_diagnostic()
}

fn open_vector_object<T: Debug + Invariant>(id: ObjID) -> Result<VecObject<T, VecObjectAlloc>> {
    Ok(VecObject::from(
        Object::map(id, MapFlags::PERSIST | MapFlags::READ | MapFlags::WRITE).into_diagnostic()?,
    ))
}

#[derive(clap::Parser, Clone, Copy, Debug)]
struct Cli {
    sub: SubCommand,
    #[clap(default_value = "u32")]
    ty: VecTy,
}

#[derive(clap::ValueEnum, clap::Parser, Clone, Copy, Debug)]
enum VecTy {
    U32,
    Foo,
}

#[derive(clap::ValueEnum, clap::Subcommand, Clone, Copy, Debug)]
enum SubCommand {
    New,
    Push,
    Append,
    Read,
}

fn open_or_create_vector_object<T: Debug + Invariant>(
    name: &str,
) -> Result<VecObject<T, VecObjectAlloc>> {
    let mut nh = naming::static_naming_factory().unwrap();
    let name = format!("/data/ptest-obj-{}", name);
    let vo = if let Ok(node) = nh.get(&name, GetFlags::empty()) {
        println!("reopened: {:?}", node.id);
        open_vector_object::<T>(node.id)
    } else {
        let vo = create_vector_object::<T>()?;
        println!("new: {:?}", vo.object().id());
        let _ = nh.remove(&name);
        nh.put(&name, vo.object().id()).into_diagnostic()?;
        Ok(vo)
    };
    vo
}

fn do_push(vo: VecObject<u32, VecObjectAlloc>) {
    let mut vo = vo;
    vo.push(vo.len() as u32).unwrap();
}

fn do_append(vo: VecObject<u32, VecObjectAlloc>) {
    let mut vo = vo;
    for i in 0..100 {
        vo.push(i).unwrap();
    }
}

fn do_read<T: Debug + Invariant>(vo: VecObject<T, VecObjectAlloc>) {
    for i in vo.iter().enumerate() {
        println!("entry {}: {:?}", i.0, i.1);
    }
}

fn main() {
    let cli = Cli::parse();
    println!("==> {:?}", cli);

    let mut nh = naming::static_naming_factory().unwrap();
    match cli.sub {
        SubCommand::New => {
            let _ = nh.remove("/data/ptest-obj-u32");
            let vo = create_vector_object::<u32>().unwrap();
            println!("new: {:?}", vo.object().id());
            nh.put("/data/ptest-obj-u32", vo.object().id()).unwrap();
        }
        SubCommand::Push => {
            let vo = open_or_create_vector_object::<u32>("u32").unwrap();
            let start = std::time::Instant::now();
            do_push(vo);
            let end = std::time::Instant::now();
            println!("done!: {:?}", end - start);
        }
        SubCommand::Append => {
            let vo = open_or_create_vector_object::<u32>("u32").unwrap();
            let start = std::time::Instant::now();
            do_append(vo);
            let end = std::time::Instant::now();
            println!("done!: {:?}", end - start);
        }
        SubCommand::Read => {
            let vo = open_or_create_vector_object::<u32>("u32").unwrap();
            let start = std::time::Instant::now();
            do_read(vo);
            let end = std::time::Instant::now();
            println!("done!: {:?}", end - start);
        }
    }

    /*
    let vo = if let Ok(node) = nh.get("/data/ptest-obj-foo", GetFlags::empty()) {
        println!("reopened: {:?}", node.id);
        open_vector_object::<Foo>(node.id).unwrap()
    } else {
        let vo = create_vector_object().unwrap();
        println!("new: {:?}", vo.object().id());
        nh.put("/data/ptest-obj-foo", vo.object().id()).unwrap();
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
    */
}
