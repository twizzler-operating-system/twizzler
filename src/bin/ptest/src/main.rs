use std::fmt::Debug;

use clap::Parser;
use miette::{IntoDiagnostic, Result};
use naming::GetFlags;
use tracing::Level;
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
use twizzler_rt_abi::{error::TwzError, object::ObjectHandle};

#[allow(dead_code)]
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

fn create_arena() -> Result<ArenaObject> {
    let obj = ObjectBuilder::default().persist();
    ArenaObject::new(obj).into_diagnostic()
}

fn open_arena(id: ObjID) -> Result<ArenaObject> {
    ArenaObject::from_objid(id).into_diagnostic()
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

fn open_or_create_arena() -> Result<ArenaObject> {
    let mut nh = naming::static_naming_factory().unwrap();
    let name = format!("/data/ptest-arena");
    let vo = if let Ok(node) = nh.get(&name, GetFlags::empty()) {
        println!("reopened-arena: {:?}", node.id);
        open_arena(node.id)
    } else {
        let vo = create_arena()?;
        println!("new-arena: {:?}", vo.object().id());
        let _ = nh.remove(&name);
        nh.put(&name, vo.object().id()).into_diagnostic()?;
        Ok(vo)
    };
    vo
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

impl Foo {
    fn new_in(
        place: impl AsRef<ObjectHandle>,
        val: u32,
        alloc: ArenaAllocator,
    ) -> Result<Self, TwzError> {
        Ok(Self {
            data: InvBox::new_in(place, val, alloc)?,
            local_data: val,
        })
    }
}

fn do_push_foo(mut vo: VecObject<Foo, VecObjectAlloc>, arena: ArenaObject) {
    let val = vo.len() as u32;
    vo.push_ctor(|r| {
        let foo = Foo::new_in(&r, val, arena.allocator())?;
        Ok(r.write(foo))
    })
    .unwrap();
}

fn do_append_foo(mut vo: VecObject<Foo, VecObjectAlloc>, arena: ArenaObject) {
    for i in 0..100 {
        vo.push_ctor(|r| {
            let foo = Foo::new_in(&r, i, arena.allocator())?;
            Ok(r.write(foo))
        })
        .unwrap();
    }
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
    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .finish(),
    )
    .unwrap();
    let cli = Cli::parse();
    println!("==> {:?}", cli);

    let mut nh = naming::static_naming_factory().unwrap();
    match cli.sub {
        SubCommand::New => match cli.ty {
            VecTy::U32 => {
                let _ = nh.remove("/data/ptest-obj-u32");
                let vo = create_vector_object::<u32>().unwrap();
                println!("new: {:?}", vo.object().id());
                nh.put("/data/ptest-obj-u32", vo.object().id()).unwrap();
            }
            VecTy::Foo => {
                let _ = nh.remove("/data/ptest-obj-foo");
                let _ = nh.remove("/data/ptest-arena");
                let vo = create_vector_object::<u32>().unwrap();
                println!("new: {:?}", vo.object().id());
                nh.put("/data/ptest-obj-foo", vo.object().id()).unwrap();

                let vo = create_arena().unwrap();
                println!("new arena: {:?}", vo.object().id());
                nh.put("/data/ptest-arena", vo.object().id()).unwrap();
            }
        },
        SubCommand::Push => match cli.ty {
            VecTy::U32 => {
                let vo = open_or_create_vector_object::<u32>("u32").unwrap();
                let start = std::time::Instant::now();
                do_push(vo);
                let end = std::time::Instant::now();
                println!("done!: {:?}", end - start);
            }
            VecTy::Foo => {
                let vo = open_or_create_vector_object::<Foo>("foo").unwrap();
                let arena = open_or_create_arena().unwrap();
                let start = std::time::Instant::now();
                do_push_foo(vo, arena);
                let end = std::time::Instant::now();
                println!("done!: {:?}", end - start);
            }
        },
        SubCommand::Append => {
            let vo = open_or_create_vector_object::<u32>("u32").unwrap();
            let start = std::time::Instant::now();
            match cli.ty {
                VecTy::U32 => do_append(vo),
                VecTy::Foo => {
                    let vo = open_or_create_vector_object::<Foo>("foo").unwrap();
                    let arena = open_or_create_arena().unwrap();
                    do_append_foo(vo, arena)
                }
            }
            let end = std::time::Instant::now();
            println!("done!: {:?}", end - start);
        }
        SubCommand::Read => match cli.ty {
            VecTy::U32 => {
                let vo = open_or_create_vector_object::<u32>("u32").unwrap();
                let start = std::time::Instant::now();
                do_read(vo);
                let end = std::time::Instant::now();
                println!("done!: {:?}", end - start);
            }
            VecTy::Foo => {
                let vo = open_or_create_vector_object::<Foo>("foo").unwrap();
                let start = std::time::Instant::now();
                do_read(vo);
                let end = std::time::Instant::now();
                println!("done!: {:?}", end - start);
            }
        },
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
