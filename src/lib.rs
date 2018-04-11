#![allow(unused_imports)]

#![feature(alloc_system)]
extern crate alloc_system;

extern crate pairing;
extern crate bellman;
extern crate rand;

use pairing::*;
use pairing::bls12_381::{Fr, Bls12};
use bellman::*;
use rand::{Rng, Rand, thread_rng};


// Synthesize the constants for each base pattern.
fn synth<E: Engine>(
    window_size: usize,
    constants: &[E::Fr],
    assignment: &mut [E::Fr]
)
{
    assert_eq!(constants.len(), 1 << window_size);
    assert_eq!(assignment.len(), 1 << window_size);
    let mut v = vec![E::Fr::zero(); 1 << window_size];

    for (i, constant) in constants.iter().enumerate() {
        let mut cur = v[i];
        cur.negate();
        cur.add_assign(constant);
        assignment[i] = cur;
        for (j, eval) in v.iter_mut().enumerate().skip(i + 1) {
            if j & i == i {
                eval.add_assign(&cur);
            }
        }
    }
}

#[test]
fn test_synth() {
    let rng = &mut thread_rng();

    let window_size = 4;

    let mut assignment = vec![Fr::zero(); (1 << window_size)];
    let constants: Vec<_> = (0..(1 << window_size)).map(|_| Fr::rand(rng)).collect();

    synth::<Bls12>(window_size, &constants, &mut assignment);

    for b in 0..(1 << window_size) {
        let mut acc = Fr::zero();

        for j in 0..(1 << window_size) {
            if j & b == j {
                acc.add_assign(&assignment[j]);
            }
        }

        assert_eq!(acc, constants[b]);
    }
}

#[derive(Copy, Clone)]
pub enum Assignment<T> {
    Known(T),
    Unknown
}

impl<T> Assignment<T> {
    pub fn unknown() -> Assignment<T> {
        Assignment::Unknown
    }

    pub fn known(v: T) -> Assignment<T> {
        Assignment::Known(v)
    }

    pub fn get(&self) -> Result<&T, Error> {
        match *self {
            Assignment::Known(ref v) => Ok(v),
            Assignment::Unknown => Err(Error::AssignmentMissing)
        }
    }
}

#[derive(Copy, Clone)]
pub struct Bit(Variable, Assignment<bool>);

impl Bit {
    pub fn one<E: Engine, CS: ConstraintSystem<E>>(_: &mut CS) -> Bit {
        Bit(CS::one(), Assignment::known(true))
    }

    pub fn alloc<E: Engine, CS: ConstraintSystem<E>>(
        cs: &mut CS,
        value: Assignment<bool>
    ) -> Result<Bit, Error>
    {
        let var = cs.alloc(|| {
            if *value.get()? {
                Ok(E::Fr::one())
            } else {
                Ok(E::Fr::zero())
            }
        })?;

        // Constrain: (1 - a) * a = 0
        cs.enforce(
            LinearCombination::zero() + CS::one() - var,
            LinearCombination::zero() + var,
            LinearCombination::zero()
        );

        Ok(Bit(var, value))
    }

    fn and<E, CS>(&self, cs: &mut CS, other: &Bit) -> Result<Bit, Error>
        where E: Engine, CS: ConstraintSystem<E>
    {
        match (*self, *other) {
            (Bit(a_var, a_val), Bit(b_var, b_val)) => {
                let mut c_val = Assignment::unknown();

                let c_var = cs.alloc(|| {
                    if *a_val.get()? && *b_val.get()? {
                        c_val = Assignment::known(true);
                        Ok(E::Fr::one())
                    } else {
                        c_val = Assignment::known(false);
                        Ok(E::Fr::zero())
                    }
                })?;

                cs.enforce(
                    LinearCombination::zero() + a_var,
                    LinearCombination::zero() + b_var,
                    LinearCombination::zero() + c_var
                );

                Ok(Bit(c_var, c_val))
            }
        }
    }
}

pub struct Num<E: Engine> {
    value: Assignment<E::Fr>,
    var: Variable
}

fn assignment_into_bits<E: Engine, CS: ConstraintSystem<E>>(num: &Assignment<E::Fr>, cs: &mut CS) -> Result<Vec<Bit>, Error>
{
    Ok(match num.get() {
        Ok(num) => {
            let mut res_assignment = vec![];
            for b in BitIterator::new(num.into_repr()) {
                res_assignment.push(Assignment::known(b));
            }
            res_assignment.reverse();
            res_assignment.truncate(E::Fr::num_bits() as usize);

            let mut res_bits = vec![];
            for b in res_assignment {
                res_bits.push(Bit::alloc(cs, b)?);
            }
            res_bits
        },
        Err(_) => {
            let mut res_bits = vec![];

            for _ in 0..E::Fr::num_bits() {
                res_bits.push(Bit::alloc(cs, Assignment::unknown())?);
            }

            res_bits
        }
    })
}

impl<E: Engine> Num<E> {
    pub fn unpack<CS: ConstraintSystem<E>>(
        &self,
        cs: &mut CS
    ) -> Result<Vec<Bit>, Error>
    {
        let bits = assignment_into_bits(&self.value, cs)?;

        let mut lc = LinearCombination::zero();

        let mut cur = E::Fr::one();
        let two = E::Fr::from_str("2").unwrap();
        for b in &bits {
            lc = lc + (cur, b.0);
            cur.mul_assign(&two);
        }

        lc = lc - self.var;

        cs.enforce(
            LinearCombination::zero(),
            LinearCombination::zero(),
            lc
        );

        assert_less_than_r(&bits, cs)?;

        Ok(bits)
    }

    fn mul<CS: ConstraintSystem<E>>(
        &self,
        cs: &mut CS,
        other: &Num<E>
    ) -> Result<Num<E>, Error>
    {
        let mut result_value = Assignment::unknown();
        let result_var = cs.alloc(|| {
            let mut e = *self.value.get()?;
            e.mul_assign(other.value.get()?);

            result_value = Assignment::known(e);

            Ok(e)
        })?;

        cs.enforce(
            LinearCombination::zero() + self.var,
            LinearCombination::zero() + other.var,
            LinearCombination::zero() + result_var
        );

        Ok(Num {
            value: result_value,
            var: result_var
        })
    }
}

impl<E: Engine> Clone for Num<E> {
    fn clone(&self) -> Num<E> {
        Num {
            value: self.value,
            var: self.var
        }
    }
}

fn coordinate_lookup<E: Engine, CS: ConstraintSystem<E>>(
    cs: &mut CS,
    table: &[E::Fr],
    bits: &[Bit],
    a: Bit,
    b: Bit,
    c: Bit,
    d: Bit
) -> Result<Num<E>, Error>
{
    assert_eq!(bits.len(), 4);
    assert_eq!(table.len(), 16);

    // The result variable
    let mut r_val = Assignment::unknown();

    let r = cs.alloc(|| {
        let mut idx = 0;

        for b in bits.iter().rev() {
            idx <<= 1;

            if *b.1.get()? {
                idx |= 1;
            }
        }

        r_val = Assignment::known(table[idx]);

        Ok(table[idx])
    })?;

    let mut constants = vec![E::Fr::zero(); 16];
    synth::<E>(4, table, &mut constants);

    let mut lhs_terms = LinearCombination::zero();

    lhs_terms = lhs_terms + (constants[0b0001], CS::one());
    lhs_terms = lhs_terms + (constants[0b0011], bits[1].0);
    lhs_terms = lhs_terms + (constants[0b0101], bits[2].0);
    lhs_terms = lhs_terms + (constants[0b0111], a.0);
    lhs_terms = lhs_terms + (constants[0b1001], bits[3].0);
    lhs_terms = lhs_terms + (constants[0b1011], b.0);
    lhs_terms = lhs_terms + (constants[0b1101], c.0);
    lhs_terms = lhs_terms + (constants[0b1111], d.0);

    let mut rhs_terms = LinearCombination::zero() + r;

    rhs_terms = rhs_terms - (constants[0b0000], CS::one());
    rhs_terms = rhs_terms - (constants[0b0010], bits[1].0);
    rhs_terms = rhs_terms - (constants[0b0100], bits[2].0);
    rhs_terms = rhs_terms - (constants[0b0110], a.0);
    rhs_terms = rhs_terms - (constants[0b1000], bits[3].0);
    rhs_terms = rhs_terms - (constants[0b1010], b.0);
    rhs_terms = rhs_terms - (constants[0b1100], c.0);
    rhs_terms = rhs_terms - (constants[0b1110], d.0);

    cs.enforce(
        lhs_terms,
        LinearCombination::zero() + bits[0].0,
        rhs_terms
    );

    Ok(Num {
        value: r_val,
        var: r
    })
}

fn point_lookup<E: Engine, CS: ConstraintSystem<E>>(
    cs: &mut CS,
    x_table: &[E::Fr],
    y_table: &[E::Fr],
    bits: &[Bit]
) -> Result<(Num<E>, Num<E>), Error>
    where E: Engine
{
    assert_eq!(bits.len(), 4);
    assert_eq!(x_table.len(), 16);
    assert_eq!(y_table.len(), 16);

    // Three values need to be precomputed:

    let a = bits[1].and(cs, &bits[2])?; // 0110
    let b = bits[3].and(cs, &bits[1])?; // 1010
    let c = bits[3].and(cs, &bits[2])?; // 1100
    let d = c.and(cs, &bits[1])?;       // 1110

    let x_coord = coordinate_lookup(cs, x_table, bits, a, b, c, d)?;
    let y_coord = coordinate_lookup(cs, y_table, bits, a, b, c, d)?;

    Ok((x_coord, y_coord))
}

#[test]
fn test_lookup() {
    use bellman::groth16::*;
    use pairing::bls12_381::{Bls12, Fr};

    let rng = &mut thread_rng();

    let x_table = (0..16).map(|_| Fr::rand(rng)).collect::<Vec<_>>();
    let y_table = (0..16).map(|_| Fr::rand(rng)).collect::<Vec<_>>();

    struct MyLookupCircuit<'a> {
        b0: Assignment<bool>,
        b1: Assignment<bool>,
        b2: Assignment<bool>,
        b3: Assignment<bool>,
        x_table: &'a [Fr],
        y_table: &'a [Fr]
    }

    impl<'a> MyLookupCircuit<'a> {
        fn blank(x_table: &'a [Fr], y_table: &'a [Fr]) -> MyLookupCircuit<'a> {
            MyLookupCircuit {
                b0: Assignment::unknown(),
                b1: Assignment::unknown(),
                b2: Assignment::unknown(),
                b3: Assignment::unknown(),
                x_table: x_table,
                y_table: y_table
            }
        }

        fn new(a: bool, b: bool, c: bool, d: bool, x_table: &'a [Fr], y_table: &'a [Fr]) -> MyLookupCircuit<'a> {
            MyLookupCircuit {
                b0: Assignment::known(a),
                b1: Assignment::known(b),
                b2: Assignment::known(c),
                b3: Assignment::known(d),
                x_table: x_table,
                y_table: y_table
            }
        }
    }

    struct MyLookupCircuitInput<E: Engine> {
        x: Num<E>,
        y: Num<E>
    }

    impl<E: Engine> Input<E> for MyLookupCircuitInput<E> {
        fn synthesize<CS: PublicConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), Error>
        {
            let x_input = cs.alloc_input(|| {
                Ok(*self.x.value.get()?)
            })?;

            let y_input = cs.alloc_input(|| {
                Ok(*self.y.value.get()?)
            })?;

            cs.enforce(
                LinearCombination::zero() + self.x.var,
                LinearCombination::zero() + CS::one(),
                LinearCombination::zero() + x_input
            );

            cs.enforce(
                LinearCombination::zero() + self.y.var,
                LinearCombination::zero() + CS::one(),
                LinearCombination::zero() + y_input
            );

            Ok(())
        }
    }

    impl<'a> Circuit<Bls12> for MyLookupCircuit<'a> {
        type InputMap = MyLookupCircuitInput<Bls12>;

        fn synthesize<CS: ConstraintSystem<Bls12>>(self, cs: &mut CS) -> Result<Self::InputMap, Error>
        {
            let b0 = Bit::alloc(cs, self.b0)?;
            let b1 = Bit::alloc(cs, self.b1)?;
            let b2 = Bit::alloc(cs, self.b2)?;
            let b3 = Bit::alloc(cs, self.b3)?;

            let bits = vec![b0, b1, b2, b3];

            let (x, y) = point_lookup(cs, self.x_table, self.y_table, &bits)?;

            Ok(MyLookupCircuitInput {
                x: x,
                y: y
            })
        }
    }

    let params = generate_random_parameters::<Bls12, _, _>(MyLookupCircuit::blank(&x_table, &y_table), rng).unwrap();

    let prepared_vk = prepare_verifying_key(&params.vk);

    for i in 0..16 {
        let proof = create_random_proof::<Bls12, _, _, _>(MyLookupCircuit::new(
            i & (1 << 0) != 0, i & (1 << 1) != 0, i & (1 << 2) != 0, i & (1 << 3) != 0, &x_table, &y_table), &params, rng).unwrap();

        assert!(verify_proof(&prepared_vk, &proof, |cs| {
            let x_var = cs.alloc(|| Ok(x_table[i]))?;
            let y_var = cs.alloc(|| Ok(y_table[i]))?;

            Ok(MyLookupCircuitInput {
                x: Num { var: x_var, value: Assignment::known(x_table[i]) },
                y: Num { var: y_var, value: Assignment::known(y_table[i]) }
            })
        }).unwrap());
    }
}

pub struct JubJub {
    // 40962
    //a: Fr,
    // -(10240/10241)
    d: Fr,
    // sqrt(-40964)
    //s: Fr
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Point {
    x: Fr,
    y: Fr
}

impl Default for JubJub {
    fn default() -> Self {
        Self::new()
    }
}

impl JubJub {
    pub fn new() -> JubJub {
        JubJub {
            //a: Fr::from_str("40962").unwrap(),
            d: Fr::from_str("19257038036680949359750312669786877991949435402254120286184196891950884077233").unwrap(),
            //s: Fr::from_str("17814886934372412843466061268024708274627479829237077604635722030778476050649").unwrap()
        }

    }
}

impl Point {
    pub fn rand<R: Rng>(rng: &mut R, j: &JubJub) -> Point {
        loop {
            let y = Fr::rand(rng);

            let mut y2 = y;
            y2.square();

            let mut n = y2;
            n.sub_assign(&Fr::one());

            let mut d = y2;
            d.mul_assign(&j.d);
            d.add_assign(&Fr::one());
            n.mul_assign(&d.inverse().unwrap());

            if let Some(x) = n.sqrt() {
                let mut tmp = Point {
                    x: x,
                    y: y
                };

                assert!(tmp.is_on_curve(j)); 
                tmp.double(j);
                tmp.double(j);
                tmp.double(j);

                // let mut tmp2 = tmp;
                // tmp2.mul_assign(&Fr::from_str("6554484396890773809930967563523245729705921265872317281365359162392183254199").unwrap(), j);
                // assert!(tmp2 == Point::zero());

                return tmp;
            }
        }
    }

    pub fn zero() -> Point {
        Point {
            x: Fr::zero(),
            y: Fr::one()
        }
    }

    pub fn is_on_curve(&self, j: &JubJub) -> bool {
        let mut x2 = self.x;
        x2.square();
        let mut lhs = x2;
        lhs.negate();
        let mut y2 = self.y;
        y2.square();
        lhs.add_assign(&y2);

        let mut rhs = j.d;
        rhs.mul_assign(&x2);
        rhs.mul_assign(&y2);
        rhs.add_assign(&Fr::one());

        lhs == rhs
    }

    pub fn add_assign(&mut self, other: &Self, j: &JubJub) {
        let mut y1y2 = self.y;
        y1y2.mul_assign(&other.y);
        let mut x1x2 = self.x;
        x1x2.mul_assign(&other.x);
        let mut dx1x2y1y2 = j.d;
        dx1x2y1y2.mul_assign(&y1y2);
        dx1x2y1y2.mul_assign(&x1x2);

        let mut d1 = dx1x2y1y2;
        d1.add_assign(&Fr::one());
        d1 = d1.inverse().unwrap();

        let mut d2 = dx1x2y1y2;
        d2.negate();
        d2.add_assign(&Fr::one());
        d2 = d2.inverse().unwrap();

        let mut x1y2 = self.x;
        x1y2.mul_assign(&other.y);

        let mut y1x2 = self.y;
        y1x2.mul_assign(&other.x);

        let mut x = x1y2;
        x.add_assign(&y1x2);
        x.mul_assign(&d1);

        let mut y = y1y2;
        y.add_assign(&x1x2);
        y.mul_assign(&d2);

        self.x = x;
        self.y = y;
    }

    pub fn double(&mut self, j: &JubJub) {
        let tmp = *self;

        self.add_assign(&tmp, j);
    }

    pub fn mul_assign(&mut self, by: &Fr, j: &JubJub) {
        let mut r = Self::zero();

        for bit in BitIterator::new(by.into_repr()) {
            r.double(j);

            if bit {
                r.add_assign(self, j);
            }
        }

        *self = r;
    }
}

#[test]
fn get_random_points() {
    let rng = &mut thread_rng();

    let j = JubJub::new();

    for _ in 0..100 {
        let p = Point::rand(rng, &j);
    }
}

pub fn generate_constant_table<R>(rng: &mut R, j: &JubJub)
    -> Vec<(Vec<Fr>, Vec<Fr>)>
    where R: Rng
{
    let points = (0..128*16).map(|_| Point::rand(rng, j)).collect::<Vec<_>>();

    points.chunks(16).map(|p| {
        let mut x_table = vec![];
        let mut y_table = vec![];

        for p in p {
            x_table.push(p.x);
            y_table.push(p.y);
        }

        (x_table, y_table)
    }).collect::<Vec<_>>()
}

pub fn pedersen_hash<CS>(
    cs: &mut CS,
    bits: &[Bit],
    generators: &[(Vec<Fr>, Vec<Fr>)],
    j: &JubJub
) -> Result<Num<Bls12>, Error>
    where CS: ConstraintSystem<Bls12>
{
    assert_eq!(bits.len(), 512);
    assert_eq!(generators.len(), (512/4));

    let mut lookups = vec![];

    for (fourbits, &(ref x_table, ref y_table)) in bits.chunks(4).zip(generators.iter()) {
        assert_eq!(x_table.len(), 16);
        assert_eq!(y_table.len(), 16);

        lookups.push(point_lookup(cs, x_table, y_table, fourbits)?);
    }

    assert_eq!(lookups.len(), generators.len());

    let mut cur_x = lookups[0].0.clone();
    let mut cur_y = lookups[0].1.clone();

    for (i, (next_x, next_y)) in lookups.into_iter().skip(1).enumerate() {
        let x1y2 = cur_x.mul(cs, &next_y)?;
        let y1x2 = cur_y.mul(cs, &next_x)?;
        let y1y2 = cur_y.mul(cs, &next_y)?;
        let x1x2 = cur_x.mul(cs, &next_x)?;
        let tau = y1y2.mul(cs, &x1x2)?;

        // We don't need to compute x for the last
        // one.
        if i != (generators.len() - 1) {
            let mut x3_val = Assignment::unknown();
            let x3 = cs.alloc(|| {
                let mut numerator = *x1y2.value.get()?;
                numerator.add_assign(y1x2.value.get()?);

                let mut denominator = *tau.value.get()?;
                denominator.mul_assign(&j.d);
                denominator.add_assign(&Fr::one());

                numerator.mul_assign(&denominator.inverse().unwrap());

                x3_val = Assignment::known(numerator);

                Ok(numerator)
            })?;

            cs.enforce(
                LinearCombination::zero() + CS::one() + (j.d, tau.var),
                LinearCombination::zero() + x3,
                LinearCombination::zero() + x1y2.var + y1x2.var
            );

            cur_x = Num {
                value: x3_val,
                var: x3
            };
        }

        let mut y3_val = Assignment::unknown();
        let y3 = cs.alloc(|| {
            let mut numerator = *x1x2.value.get()?;
            numerator.add_assign(y1y2.value.get()?);

            let mut denominator = *tau.value.get()?;
            denominator.mul_assign(&j.d);
            denominator.negate();
            denominator.add_assign(&Fr::one());

            numerator.mul_assign(&denominator.inverse().unwrap());

            y3_val = Assignment::known(numerator);

            Ok(numerator)
        })?;

        cs.enforce(
            LinearCombination::zero() + CS::one() - (j.d, tau.var),
            LinearCombination::zero() + y3,
            LinearCombination::zero() + x1x2.var + y1y2.var
        );

        cur_y = Num {
            value: y3_val,
            var: y3
        };
    }

    Ok(cur_y)
}

#[test]
fn test_pedersen() {
    use bellman::groth16::*;
    use pairing::bls12_381::{Bls12, Fr};

    let rng = &mut thread_rng();

    struct MyLookupCircuit<'a> {
        bits: Vec<Assignment<bool>>,
        generators: &'a[(Vec<Fr>, Vec<Fr>)],
        j: &'a JubJub
    }

    impl<'a> MyLookupCircuit<'a> {
        fn blank(generators: &'a [(Vec<Fr>, Vec<Fr>)], j: &'a JubJub) -> MyLookupCircuit<'a> {
            MyLookupCircuit {
                bits: (0..512).map(|_| Assignment::unknown()).collect(),
                generators: generators,
                j: j
            }
        }

        fn new(
            generators: &'a [(Vec<Fr>, Vec<Fr>)],
            bits: &[bool],
            j: &'a JubJub
        ) -> MyLookupCircuit<'a>
        {
            assert_eq!(bits.len(), 512);

            MyLookupCircuit {
                bits: bits.iter().map(|&b| Assignment::known(b)).collect(),
                generators: generators,
                j: j
            }
        }
    }

    struct MyLookupCircuitInput<E: Engine> {
        r: Num<E>
    }

    impl<E: Engine> Input<E> for MyLookupCircuitInput<E> {
        fn synthesize<CS: PublicConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), Error>
        {
            let r_input = cs.alloc_input(|| {
                Ok(*self.r.value.get()?)
            })?;

            cs.enforce(
                LinearCombination::zero() + self.r.var,
                LinearCombination::zero() + CS::one(),
                LinearCombination::zero() + r_input
            );

            Ok(())
        }
    }

    impl<'a> Circuit<Bls12> for MyLookupCircuit<'a> {
        type InputMap = MyLookupCircuitInput<Bls12>;

        fn synthesize<CS: ConstraintSystem<Bls12>>(self, cs: &mut CS) -> Result<Self::InputMap, Error>
        {
            let mut bits = Vec::with_capacity(512);
            for b in self.bits.into_iter() {
                bits.push(Bit::alloc(cs, b)?);
            }

            let res = pedersen_hash(cs, &bits, self.generators, self.j)?;

            Ok(MyLookupCircuitInput {
                r: res
            })
        }
    }

    let j = JubJub::new();
    let generators = generate_constant_table(rng, &j);
    let params = generate_random_parameters::<Bls12, _, _>(MyLookupCircuit::blank(&generators, &j), rng).unwrap();
    let prepared_vk = prepare_verifying_key(&params.vk);

    let bits = (0..512).map(|_| rng.gen()).collect::<Vec<bool>>();

    let proof = create_random_proof::<Bls12, _, _, _>(MyLookupCircuit::new(
        &generators,
        &bits,
        &j
    ), &params, rng).unwrap();

    let expected_result = {
        let mut cur = Point::zero();

        for (chunk, g) in bits.chunks(4).zip(generators.iter()) {
            let mut idx = 0;
            for c in chunk.iter().rev() {
                idx <<= 1;

                if *c {
                    idx |= 1;
                }
            }

            let new_point = Point {
                x: g.0[idx],
                y: g.1[idx]
            };

            cur.add_assign(&new_point, &j);
        }

        cur.y
    };

    assert!(verify_proof(&prepared_vk, &proof, |cs| {
        let r_var = cs.alloc(|| Ok(expected_result))?;

        Ok(MyLookupCircuitInput {
            r: Num { var: r_var, value: Assignment::known(expected_result) }
        })
    }).unwrap());
}

/// A boolean object that's rather fun! (way too tired for this)
#[derive(Clone, Copy)]
enum FunBit {
    Constant(bool),
    Is(Variable, Assignment<bool>),
    Not(Variable, Assignment<bool>)
}

impl FunBit {
    fn from_bit(b: Bit) -> FunBit {
        FunBit::Is(b.0, b.1)
    }

    fn assert_is_false<E: Engine, CS: ConstraintSystem<E>>(&self, cs: &mut CS)
    {
        match *self {
            FunBit::Constant(false) => {},
            FunBit::Constant(true) => panic!("is false when shouldn't be"),
            FunBit::Is(is_var, _) => {
                cs.enforce(
                    LinearCombination::zero() + is_var,
                    LinearCombination::zero() + CS::one(),
                    LinearCombination::zero()
                );
            },
            FunBit::Not(not_var, _) => {
                cs.enforce(
                    LinearCombination::zero() + CS::one() - not_var,
                    LinearCombination::zero() + CS::one(),
                    LinearCombination::zero()
                );
            }
        }
    }

    fn not(&self) -> FunBit {
        match *self {
            FunBit::Constant(b) => FunBit::Constant(!b),
            FunBit::Is(var, val) => FunBit::Not(var, val),
            FunBit::Not(var, val) => FunBit::Is(var, val)
        }
    }

    fn or<E: Engine, CS: ConstraintSystem<E>>(&self, other: &FunBit, cs: &mut CS)
        -> Result<FunBit, Error>
    {
        Ok(self.not().and(&other.not(), cs)?.not())
    }

    fn xor<E: Engine, CS: ConstraintSystem<E>>(&self, other: &FunBit, cs: &mut CS)
        -> Result<FunBit, Error>
    {
        Ok(match (*self, *other) {
            (FunBit::Constant(false), a) | (a, FunBit::Constant(false)) => {
                a
            },
            (FunBit::Constant(true), a) | (a, FunBit::Constant(true)) => {
                a.not()
            },
            (FunBit::Is(a_var, a_val), FunBit::Is(b_var, b_val)) |
            (FunBit::Not(a_var, a_val), FunBit::Not(b_var, b_val)) => {
                let mut c_val = Assignment::unknown();

                let c_var = cs.alloc(|| {
                    if (*a_val.get()?) ^ (*b_val.get()?) {
                        c_val = Assignment::known(true);

                        Ok(E::Fr::one())
                    } else {
                        c_val = Assignment::known(false);

                        Ok(E::Fr::zero())
                    }
                })?;

                cs.enforce(
                    LinearCombination::zero() + a_var + a_var,
                    LinearCombination::zero() + b_var,
                    LinearCombination::zero() + a_var + b_var - c_var
                );

                FunBit::Is(c_var, c_val)
            }
            (FunBit::Is(is_var, is_val), FunBit::Not(not_var, not_val)) |
            (FunBit::Not(not_var, not_val), FunBit::Is(is_var, is_val)) => {
                FunBit::Is(is_var, is_val).xor(&FunBit::Is(not_var, not_val), cs)?.not()
            }
        })
    }

    fn and<E: Engine, CS: ConstraintSystem<E>>(&self, other: &FunBit, cs: &mut CS)
        -> Result<FunBit, Error>
    {
        Ok(match (*self, *other) {
            (FunBit::Constant(false), _) | (_, FunBit::Constant(false)) => {
                FunBit::Constant(false)
            },
            (FunBit::Constant(true), a) | (a, FunBit::Constant(true)) => {
                a
            },
            (FunBit::Is(a_var, a_val), FunBit::Is(b_var, b_val)) => {
                let mut c_val = Assignment::unknown();

                let c_var = cs.alloc(|| {
                    if (*a_val.get()?) && (*b_val.get()?) {
                        c_val = Assignment::known(true);
                        Ok(E::Fr::one())
                    } else {
                        c_val = Assignment::known(false);
                        Ok(E::Fr::zero())
                    }
                })?;

                cs.enforce(
                    LinearCombination::zero() + a_var,
                    LinearCombination::zero() + b_var,
                    LinearCombination::zero() + c_var
                );

                FunBit::Is(c_var, c_val)
            },
            (FunBit::Not(a_var, a_val), FunBit::Not(b_var, b_val)) => {
                let mut c_val = Assignment::unknown();

                let c_var = cs.alloc(|| {
                    if (!*a_val.get()?) && (!*b_val.get()?) {
                        c_val = Assignment::known(true);
                        Ok(E::Fr::one())
                    } else {
                        c_val = Assignment::known(false);
                        Ok(E::Fr::zero())
                    }
                })?;

                cs.enforce(
                    LinearCombination::zero() + CS::one() - a_var,
                    LinearCombination::zero() + CS::one() - b_var,
                    LinearCombination::zero() + c_var
                );

                FunBit::Is(c_var, c_val)
            },
            (FunBit::Is(is_var, is_val), FunBit::Not(not_var, not_val)) |
            (FunBit::Not(not_var, not_val), FunBit::Is(is_var, is_val)) => {
                let mut c_val = Assignment::unknown();

                let c_var = cs.alloc(|| {
                    if (*is_val.get()?) && (!*not_val.get()?) {
                        c_val = Assignment::known(true);
                        Ok(E::Fr::one())
                    } else {
                        c_val = Assignment::known(false);
                        Ok(E::Fr::zero())
                    }
                })?;

                cs.enforce(
                    LinearCombination::zero() + is_var,
                    LinearCombination::zero() + CS::one() - not_var,
                    LinearCombination::zero() + c_var
                );

                FunBit::Is(c_var, c_val)
            }
        })
    }
}

/// Takes little-endian order bits, subtracts Fr and asserts
/// no carry.
fn assert_less_than_r<E: Engine, CS: ConstraintSystem<E>>(bits: &[Bit], cs: &mut CS)
    -> Result<(), Error>
{
    let mut r_bits = vec![];
    for b in BitIterator::new(Fr::char()) {
        r_bits.push(FunBit::Constant(b));
    }
    r_bits.reverse();
    r_bits.truncate(E::Fr::num_bits() as usize);

    let mut carry = FunBit::Constant(false);

    for (a, b) in r_bits.into_iter().zip(bits.iter().cloned()) {
        let b = FunBit::from_bit(b);

        let t1 = a.xor(&b, cs)?;
        let t2 = a.not().and(&b, cs)?;
        let t3 = t1.not().and(&carry, cs)?;
        let t4 = t2.or(&t3, cs)?;

        carry = t4;
    }

    // dirty and somewhat cheap
    carry.assert_is_false(cs);

    Ok(())
}

#[test]
fn testy_more_pedersen()
{
    use bellman::groth16::*;
    use pairing::bls12_381::{Bls12, Fr};

    let rng = &mut thread_rng();

    struct MyLookupCircuit<'a> {
        bits: Vec<Assignment<bool>>,
        generators: &'a[(Vec<Fr>, Vec<Fr>)],
        j: &'a JubJub
    }

    impl<'a> MyLookupCircuit<'a> {
        fn blank(generators: &'a [(Vec<Fr>, Vec<Fr>)], j: &'a JubJub) -> MyLookupCircuit<'a> {
            MyLookupCircuit {
                bits: (0..512).map(|_| Assignment::unknown()).collect(),
                generators: generators,
                j: j
            }
        }

        fn new(
            generators: &'a [(Vec<Fr>, Vec<Fr>)],
            bits: &[bool],
            j: &'a JubJub
        ) -> MyLookupCircuit<'a>
        {
            assert_eq!(bits.len(), 512);

            MyLookupCircuit {
                bits: bits.iter().map(|&b| Assignment::known(b)).collect(),
                generators: generators,
                j: j
            }
        }
    }

    struct MyLookupCircuitInput;

    impl<E: Engine> Input<E> for MyLookupCircuitInput {
        fn synthesize<CS: PublicConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), Error>
        {
            Ok(())
        }
    }

    impl<'a> Circuit<Bls12> for MyLookupCircuit<'a> {
        type InputMap = MyLookupCircuitInput;

        fn synthesize<CS: ConstraintSystem<Bls12>>(self, cs: &mut CS) -> Result<Self::InputMap, Error>
        {
            let mut bits = Vec::with_capacity(512);
            for b in self.bits.iter() {
                bits.push(Bit::alloc(cs, *b)?);
            }

            const DEPTH: usize = 50;

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

            Ok(MyLookupCircuitInput)
        }
    }

    let j = JubJub::new();
    let generators = generate_constant_table(rng, &j);
    let params = generate_random_parameters::<Bls12, _, _>(MyLookupCircuit::blank(&generators, &j), rng).unwrap();
    let prepared_vk = prepare_verifying_key(&params.vk);

    let mut bits;
    let mut proof;

    use std::time::{Duration, Instant};

    let mut elapsed = Duration::new(0, 0);

    let mut i = 0;

    loop {
        bits = (0..512).map(|_| rng.gen()).collect::<Vec<bool>>();
        let now = Instant::now();
        proof = create_random_proof::<Bls12, _, _, _>(MyLookupCircuit::new(
            &generators,
            &bits,
            &j
        ), &params, rng).unwrap();

        elapsed += now.elapsed();

        i += 1;

        if i == 15 {
            break
        }
    }

    println!("each proof took on average {:?}", elapsed / 15);

    let expected_result = {
        let mut cur = Point::zero();

        for (chunk, g) in bits.chunks(4).zip(generators.iter()) {
            let mut idx = 0;
            for c in chunk.iter().rev() {
                idx <<= 1;

                if *c {
                    idx |= 1;
                }
            }

            let new_point = Point {
                x: g.0[idx],
                y: g.1[idx]
            };

            cur.add_assign(&new_point, &j);
        }

        cur.y
    };

    assert!(verify_proof(&prepared_vk, &proof, |cs| {
        let r_var = cs.alloc(|| Ok(expected_result))?;

        Ok(MyLookupCircuitInput)
    }).unwrap());
}


// We'll use these interfaces to construct our circuit.
use bellman::{
    Circuit,
    ConstraintSystem,
    Error
};

// We're going to use the Groth16 proving system.
use bellman::groth16::{
    Proof,
    generate_random_parameters,
    prepare_verifying_key,
    create_random_proof,
    verify_proof,
};


pub const MIMC_ROUNDS: usize = 300;


pub fn mimc<E: Engine>(
    mut xl: E::Fr,
    mut xr: E::Fr,
    constants: &[E::Fr]
) -> E::Fr
{
    assert_eq!(constants.len(), MIMC_ROUNDS);

    for i in 0..MIMC_ROUNDS {
        let mut tmp1 = xl;
        tmp1.add_assign(&constants[i]);
        let mut tmp2 = tmp1;
        tmp2.square();
        tmp2.mul_assign(&tmp1);
        tmp2.add_assign(&xr);
        xr = xl;
        xl = tmp2;
    }

    xl
}

/// This is our demo circuit for proving knowledge of the
/// preimage of a MiMC hash invocation.
pub struct MiMCDemo<'a, E: Engine> {
    pub xl: Option<E::Fr>,
    pub xr: Option<E::Fr>,
    pub constants: &'a [E::Fr]
}

pub struct MiMCDemoCircuitInput;

impl<E: Engine> Input<E> for MiMCDemoCircuitInput {
    fn synthesize<CS: PublicConstraintSystem<E>>(self, _: &mut CS) -> Result<(), Error>
    {
        Ok(())
    }
}

/// Our demo circuit implements this `Circuit` trait which
/// is used during paramgen and proving in order to
/// synthesize the constraint system.
impl<'a, E: Engine> Circuit<E> for MiMCDemo<'a, E> {
    type InputMap = MiMCDemoCircuitInput;

    fn synthesize<CS: ConstraintSystem<E>>(
        self,
        cs: &mut CS
    ) -> Result<Self::InputMap, Error>
    {
        assert_eq!(self.constants.len(), MIMC_ROUNDS);

        // Allocate the first component of the preimage.
        let mut xl_value = self.xl;
        let mut xl = cs.alloc(||  {
            xl_value.ok_or(Error::AssignmentMissing)
        })?;

        // Allocate the second component of the preimage.
        let mut xr_value = self.xr;
        let mut xr = cs.alloc(||  {
            xr_value.ok_or(Error::AssignmentMissing)
        })?;

        for i in 0..MIMC_ROUNDS {
            // xL, xR := xR + (xL + Ci)^3, xL
//            let cs = &mut cs.namespace(|| format!("round {}", i));

            // tmp = (xL + Ci)^2
            let mut tmp_value = xl_value.map(|mut e| {
                e.add_assign(&self.constants[i]);
                e.square();
                e
            });
            let mut tmp = cs.alloc(||  {
                tmp_value.ok_or(Error::AssignmentMissing)
            })?;

            cs.enforce(
                LinearCombination::zero() + xl + (self.constants[i], CS::one()),
                LinearCombination::zero() + xl + (self.constants[i], CS::one()),
                LinearCombination::zero() + tmp
            );

            // new_xL = xR + (xL + Ci)^3
            // new_xL = xR + tmp * (xL + Ci)
            // new_xL - xR = tmp * (xL + Ci)
            let mut new_xl_value = xl_value.map(|mut e| {
                e.add_assign(&self.constants[i]);
                e.mul_assign(&tmp_value.unwrap());
                e.add_assign(&xr_value.unwrap());
                e
            });

            let mut new_xl = if i == (MIMC_ROUNDS-1) {
                // This is the last round, xL is our image and so
                // we allocate a public input.
                cs.alloc(||  {
                    new_xl_value.ok_or(Error::AssignmentMissing)
                })?
            } else {
                cs.alloc(||  {
                    new_xl_value.ok_or(Error::AssignmentMissing)
                })?
            };

            cs.enforce(
                LinearCombination::zero() + tmp,
                LinearCombination::zero() + xl + (self.constants[i], CS::one()),
                LinearCombination::zero() + new_xl - xr
            );

            // xR = xL
            xr = xl;
            xr_value = xl_value;

            // xL = new_xL
            xl = new_xl;
            xl_value = new_xl_value;
        }

        Ok(MiMCDemoCircuitInput)
    }
}

