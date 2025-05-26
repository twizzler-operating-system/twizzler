mod benches {
    extern crate test;
    use test::Bencher;
    use twizzler_abi::object::Protections;

    use crate::*;
    #[bench]
    fn bench_capability_creation(b: &mut Bencher) {
        let (s, v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        b.iter(|| {
            let cap = Cap::new(
                0x123.into(),
                0x321.into(),
                Protections::all(),
                s.base(),
                Revoc::default(),
                Gates::default(),
                HashingAlgo::Sha256,
                SigningScheme::Ecdsa,
            )
            .expect("Capability should have been created.");
        })
    }

    #[bench]
    fn bench_something_else(b: &mut Bencher) {
        b.iter(|| {
            let x = black_box(5 * 10);
        })
    }

    #[bench]
    fn bench_keypair_creation(b: &mut Bencher) {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
            Protections::all(),
        );

        b.iter(|| {
            let (s_obj, v_obj) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
                .expect("Keys should be generated properly");
        });
    }

    #[bench]
    fn bench_capability_verification(b: &mut Bencher) {
        let (s, v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        let cap = Cap::new(
            0x123.into(),
            0x321.into(),
            Protections::all(),
            s.base(),
            Revoc::default(),
            Gates::default(),
            HashingAlgo::Sha256,
            SigningScheme::Ecdsa,
        )
        .expect("Capability should have been created.");

        b.iter(|| {
            cap.verify_sig(v.base())
                .expect("capability should have been verified.");
        })
    }
}
