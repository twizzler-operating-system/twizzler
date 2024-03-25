/*
 * Main Entry: observer
 * Pronunciation /əbˈzərvər/
 * Function: noun
 * Etymology: From Middle French observer, from Old French observer, from Latin observō
 *      (“to watch”), from ob- (“before”) + servō (“to keep”), from Proto-Indo-European *ser- (“to guard”).
 *      Cognate with Gothic 𐍃𐌰𐍂𐍅𐌰 (sarwa, “weapons, armour”), Old English searu (“device”).
 * Date: before 17th century
 * 1 : a person who watches someone or something
 * 2 : a person who attends a meeting, lesson, etc. to listen and watch but not to take part
 * 3 : A team of officials was sent as observers to the conference.
 * 4 : a person who watches and studies particular events, situations, etc. and
 *      is therefore considered to be an expert on the a celebrity observer
*/

extern crate twizzler_abi;

use tracing::{dispatcher::set_global_default, event, info, info_span, span::{ Attributes, Record }, Dispatch, Event, Id, Level, Metadata};
use tracing::instrument;
use tracing::Subscriber;
use tracing_subscriber::{layer::{Context, SubscriberExt}, Layer};
use twizzler_object::{Object, ObjectInitFlags, ObjectInitError};
use twizzler_abi::{
    object::{ObjID, Protections, NULLPAGE_SIZE},
    syscall::{
        BackingType, LifetimeType,
        ObjectCreate, ObjectCreateFlags, sys_object_ctrl,
        ObjectControlCmd, DeleteFlags, ObjectControlError
    },
};
use std::sync::atomic::{AtomicU64, Ordering};
use observer::bumper::Bump;


pub struct MySubscriber {
    ids : AtomicU64,
}

impl MySubscriber {
    fn new() -> MySubscriber {
        MySubscriber {
            ids : AtomicU64::new(1)
        }
    }
}

impl Subscriber for MySubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let result = self.ids.fetch_add(1, Ordering::SeqCst);
        println!("Created a new span with id with id {}!", result);

        tracing::Id::from_u64(result)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        println!("Recording values in {}", span.into_u64());
    }

    fn record_follows_from(&self, span: &Id, follows: &Id) {
        println!("Tracking span {} from {}", span.into_u64(), follows.into_u64());
    }

    fn event(&self, event: &Event<'_>) {
        let _ = event;
        println!("Event happened!");
    }

    fn enter(&self, span: &Id) {
        println!("Entered a span with id {}", span.into_u64());
    }

    fn exit(&self, span: &Id) {
        println!("Exited a span with id {}", span.into_u64());
    }
}

unsafe impl Send for MySubscriber{}
unsafe impl Sync for MySubscriber{}

/*pub struct MyLayer {

}

impl MyLayer {
    fn new() -> MyLayer {
        MyLayer {}
    }
}

impl<S: Subscriber> Layer<S> for MyLayer {
    fn on_event(&self, _event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        println!("Hello world!\n");
    }

    fn on_enter(&self, _id: &Id, _ctx: Context<'_, S>) {
        println!("Entered context");
    }
}*/

fn baz() {

}

#[instrument]
fn foo() {
    println!("Doing cool stuff!");
}

fn main() {
    //let subscriber = MySubscriber::new().with(MyLayer::new());

    let subscriber = MySubscriber::new();

    tracing::subscriber::set_global_default(subscriber).expect("worky");
    let x = info_span!("Stuff");
    let _guard = x.enter();
    info!("Created");
    foo();
    foo();
    
}