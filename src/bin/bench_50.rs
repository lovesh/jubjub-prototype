// // This stuff forces rustc to avoid jemalloc so that massif can profile it easier.
// // START
//         #![feature(global_allocator, allocator_api)]

//         use std::heap::{Alloc, System, Layout, AllocErr};

//         struct MyAllocator;

//         unsafe impl<'a> Alloc for &'a MyAllocator {
//             unsafe fn alloc(&mut self, layout: Layout) -> Result<*mut u8, AllocErr> {
//                 System.alloc(layout)
//             }

//             unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
//                 System.dealloc(ptr, layout)
//             }
//         }

//         #[global_allocator]
//         static GLOBAL: MyAllocator = MyAllocator;
// // END

extern crate pairing;
extern crate bellman;
extern crate rand;
extern crate jubjub;

use bellman::groth16::*;
use pairing::*;
use pairing::bls12_381::{Fr, Bls12};
use bellman::*;
use rand::{Rng, thread_rng, XorShiftRng, SeedableRng};

use jubjub::*;

struct DemoPedersenHashCircuit<'a> {
    bits: Vec<Assignment<bool>>,
    generators: &'a[(Vec<Fr>, Vec<Fr>)],
    j: &'a JubJub
}

impl<'a> DemoPedersenHashCircuit<'a> {
    fn blank(generators: &'a [(Vec<Fr>, Vec<Fr>)], j: &'a JubJub) -> DemoPedersenHashCircuit<'a> {
        DemoPedersenHashCircuit {
            bits: (0..512).map(|_| Assignment::unknown()).collect(),
            generators: generators,
            j: j
        }
    }

    fn new(
        generators: &'a [(Vec<Fr>, Vec<Fr>)],
        bits: &[bool],
        j: &'a JubJub
    ) -> DemoPedersenHashCircuit<'a>
    {
        assert_eq!(bits.len(), 512);

        DemoPedersenHashCircuit {
            bits: bits.iter().map(|&b| Assignment::known(b)).collect(),
            generators: generators,
            j: j
        }
    }
}

struct DemoPedersenHashCircuitInput;

impl<E: Engine> Input<E> for DemoPedersenHashCircuitInput {
    fn synthesize<CS: PublicConstraintSystem<E>>(self, _: &mut CS) -> Result<(), Error>
    {
        Ok(())
    }
}

impl<'a> Circuit<Bls12> for DemoPedersenHashCircuit<'a> {
    type InputMap = DemoPedersenHashCircuitInput;

    fn synthesize<CS: ConstraintSystem<Bls12>>(self, cs: &mut CS) -> Result<Self::InputMap, Error>
    {
        let mut bits = Vec::with_capacity(512);
        for b in &self.bits {
            bits.push(Bit::alloc(cs, *b)?);
        }

        const DEPTH: usize = 20;

        for i in 0..DEPTH {
            let num = pedersen_hash(cs, &bits, self.generators, self.j)?;

            if i != (DEPTH - 1) {
                bits = num.unpack(cs)?;
                assert_eq!(bits.len(), 255);
                for b in self.bits.iter().take(255) {
                    bits.push(Bit::alloc(cs, *b)?);
                }
                bits.push(Bit::one(cs));
                bits.push(Bit::one(cs));

                assert_eq!(bits.len(), 512);
            }
        }

        Ok(DemoPedersenHashCircuitInput)
    }
}

use std::fs::File;
use std::path::Path;

fn main() {
    let rng = &mut thread_rng();
    let mut generator_rng = XorShiftRng::from_seed([0x5dbe_6259, 0x8d31_3d76, 0x3237_db17, 0xe5bc_0654]);
    let j = JubJub::new();
    println!("Creating random generators for the Pedersen hash...");
    let generators = generate_constant_table(&mut generator_rng, &j);
    println!("Done!");
    drop(generator_rng);

    if !Path::new("params").exists() {
        println!("Creating the parameters and saving them to `./params`");
        let params = generate_random_parameters::<Bls12, _, _>(DemoPedersenHashCircuit::blank(&generators, &j), rng).unwrap();
        params.write(&mut File::create("params").unwrap()).unwrap();
        println!("Just wrote the parameters to disk! We don't need to do it next time.");
    }

    use std::time::{Duration,Instant};

    let mut total_mimc = Duration::new(0, 0);

    const SAMPLES: u32 = 50;

    let constants = (0..MIMC_ROUNDS).map(|_| rng.gen()).collect::<Vec<Fr>>();

    println!("MiMC: Creating {} proofs and averaging the time spent creating them.", SAMPLES);

    for _ in 0..SAMPLES {
        let xl: Fr = rng.gen();
        let xr: Fr = rng.gen();
        let now = Instant::now();
        let bits = (0..512).map(|_| rng.gen()).collect::<Vec<bool>>();
        let c = MiMCDemo::<Bls12> {
            xl: Some(xl),
            xr: Some(xr),
            constants: &constants
        };
        create_random_proof::<Bls12, _, _, _>(c, params, rng).unwrap();
        total_mimc += now.elapsed();
    }

    let avg_mimc = total_mimc / SAMPLES;
    println!("total_mimc proving time: {:?}", total_mimc);
    println!("average_mimc proving time: {:?}", avg_mimc);

    println!("Pedersen:Creating {} proofs and averaging the time spent creating them.", SAMPLES);

    let mut total_pedersen = Duration::new(0, 0);
    for _ in 0..SAMPLES {
        let now = Instant::now();
        let params = ProverStream::new("params").unwrap();
        let bits = (0..512).map(|_| rng.gen()).collect::<Vec<bool>>();
        create_random_proof::<Bls12, _, _, _>(DemoPedersenHashCircuit::new(
            &generators,
            &bits,
            &j
        ), params, rng).unwrap();
        total_pedersen += now.elapsed();
    }
    let avg_pedersen = total_pedersen / SAMPLES;
    println!("total_pedersen proving time: {:?}", total_pedersen);
    println!("average_pedersen proving time: {:?}", avg_pedersen);
}
