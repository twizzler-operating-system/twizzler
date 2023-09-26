//! Example of an object lifetime
//! Create an object, place typed data in it, access typed data, destroy object
//! Based on example from Albert Lee

use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        BackingType, LifetimeType,
        ObjectCreate, ObjectCreateFlags, sys_object_ctrl,
        ObjectControlCmd, DeleteFlags
    },
};
use twizzler_object::{Object, ObjectInitFlags};

fn main() {

    println!("Hello, world! From objtest.");

    // specify parameters for new object
    let new_obj_spec = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );

    // create new empty object using specification
    // involves kernel operation
    let new_obj_id =  twizzler_abi::syscall::sys_object_create(
        new_obj_spec,
        &[],
        &[],
    ).unwrap();
    
    // initialize object with ID new_obj_id
    // allocate space (slot) for array of 50 32-bit int
    // get a handle for the array
    // no kernel operations?
    let obj: Object<[i32; 50]> = Object::<[i32; 50]>::init_id(
        new_obj_id,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ).unwrap();

    // Fill in the values of the array
    // no kernel operations?
    unsafe {
        let arr = obj.base_mut_unchecked();
        for i in 0..49 {
            arr[i] = i as i32;
        }
    };

    println!("id of inner object: {:?}", obj.id());
    println!("id of enclosing object: {:?}", new_obj_id);

    // print array
    // no kernel operations?
    unsafe {println!("array: {:?}", obj.base_unchecked())};

    // delete the object, ignore the result
    // involves kernel operation
    _ = sys_object_ctrl(new_obj_id, ObjectControlCmd::Delete(DeleteFlags::FORCE));

  
    //println!("Created an object at time: with value: .");
}
