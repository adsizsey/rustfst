use std::cmp::max;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::collections::HashSet;
use std::marker::PhantomData;

use anyhow::Result;
use binary_heap_plus::BinaryHeap;
use stable_bst::TreeMap;

use crate::algorithms::encode::EncodeType;
use crate::algorithms::factor_weight::factor_iterators::GallicFactorLeft;
use crate::algorithms::factor_weight::{factor_weight, FactorWeightOptions, FactorWeightType};
use crate::algorithms::partition::Partition;
use crate::algorithms::queues::LifoQueue;
use crate::algorithms::reverse;
use crate::algorithms::tr_compares::ILabelCompare;
use crate::algorithms::tr_mappers::QuantizeMapper;
use crate::algorithms::tr_unique;
use crate::algorithms::weight_converters::{FromGallicConverter, ToGallicConverter};
use crate::algorithms::Queue;
use crate::algorithms::{
    connect,
    encode::{decode, encode},
    push_weights, tr_map, tr_sort, weight_convert, ReweightType,
};
use crate::fst_impls::VectorFst;
use crate::fst_properties::FstProperties;
use crate::fst_traits::{AllocableFst, CoreFst, ExpandedFst, Fst, MutableFst};
use crate::semirings::{
    GallicWeightLeft, Semiring, SemiringProperties, WeaklyDivisibleSemiring, WeightQuantize,
};
use crate::EPS_LABEL;
use crate::KDELTA;
use crate::NO_STATE_ID;
use crate::{StateId, Trs};
use crate::{Tr, KSHORTESTDELTA};

pub fn minimize_default<W, F>(ifst: &mut F) -> Result<()>
    where
        F: MutableFst<W> + ExpandedFst<W> + AllocableFst<W>,
        W: WeaklyDivisibleSemiring + WeightQuantize,
        W::ReverseWeight: WeightQuantize
{
    minimize(ifst, KSHORTESTDELTA, false)
}


/// In place minimization of deterministic weighted automata and transducers,
/// and also non-deterministic ones if they use an idempotent semiring.
/// For transducers, the algorithm produces a compact factorization of the minimal transducer.
pub fn minimize<W, F>(
    ifst: &mut F,
    delta: f32,
    allow_nondet: bool
) -> Result<()>
where
    F: MutableFst<W> + ExpandedFst<W> + AllocableFst<W>,
    W: WeaklyDivisibleSemiring + WeightQuantize,
    W::ReverseWeight: WeightQuantize
{
    let props = ifst.compute_and_update_properties(
        FstProperties::ACCEPTOR
            | FstProperties::I_DETERMINISTIC
            | FstProperties::WEIGHTED
            | FstProperties::UNWEIGHTED,
    )?;

    let allow_acyclic_minimization = if props.contains(FstProperties::I_DETERMINISTIC) {
        true
    } else {
        if !W::properties().contains(SemiringProperties::IDEMPOTENT) {
            bail!("Cannot minimize a non-deterministic FST over a non-idempotent semiring")
        } else if !allow_nondet {
            bail!("Refusing to minimize a non-deterministic FST with allow_nondet = false")
        }

        false
    };

    if !props.contains(FstProperties::ACCEPTOR) {
        // Weighted transducer
        let mut to_gallic = ToGallicConverter {};
        // dbg!(ifst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("==============================================================================");
        println!("To Gallic");
        print_fst2(ifst);
        let mut gfst: VectorFst<GallicWeightLeft<W>> = weight_convert(ifst, &mut to_gallic)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("==============================================================================");
        println!("Push weights");
        print_fst2(&gfst);
        push_weights(&mut gfst, ReweightType::ReweightToInitial, delta, false)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        let quantize_mapper = QuantizeMapper::new(delta);
        println!("==============================================================================");
        println!("Quantize");
        print_fst2(&gfst);
        tr_map(&mut gfst, &quantize_mapper)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("==============================================================================");
        println!("Encode");
        print_fst2(&gfst);
        let encode_table = encode(&mut gfst, EncodeType::EncodeWeightsAndLabels)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("==============================================================================");
        println!("Acceptor minimize");
        print_fst2(&gfst);
        acceptor_minimize(&mut gfst, allow_acyclic_minimization)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("Decode");
        decode(&mut gfst, encode_table)?;
        // dbg!(gfst.properties().contains(FstProperties::I_DETERMINISTIC));
        let factor_opts: FactorWeightOptions = FactorWeightOptions {
            delta: KDELTA,
            mode: FactorWeightType::FACTOR_FINAL_WEIGHTS | FactorWeightType::FACTOR_ARC_WEIGHTS,
            final_ilabel: 0,
            final_olabel: 0,
            increment_final_ilabel: false,
            increment_final_olabel: false,
        };
        println!("Factorweight");
        let fwfst: VectorFst<_> =
            factor_weight::<_, VectorFst<GallicWeightLeft<W>>, _, _, GallicFactorLeft<W>>(
                &gfst,
                factor_opts,
            )?;
        // dbg!(fwfst.properties().contains(FstProperties::I_DETERMINISTIC));
        println!("From Gallic");
        let mut from_gallic = FromGallicConverter {
            superfinal_label: EPS_LABEL,
        };
        *ifst = weight_convert(&fwfst, &mut from_gallic)?;
        println!("Done");
        // dbg!(ifst.properties().contains(FstProperties::I_DETERMINISTIC));
        Ok(())
    } else if props.contains(FstProperties::WEIGHTED) {
        // Weighted acceptor
        push_weights(ifst, ReweightType::ReweightToInitial, delta, false)?;
        let quantize_mapper = QuantizeMapper::new(delta);
        tr_map(ifst, &quantize_mapper)?;
        let encode_table = encode(ifst, EncodeType::EncodeWeightsAndLabels)?;
        acceptor_minimize(ifst, allow_acyclic_minimization)?;
        decode(ifst, encode_table)
    } else {
        // Unweighted acceptor
        acceptor_minimize(ifst, allow_acyclic_minimization)
    }
}

pub fn print_fst2<W: Semiring, F: ExpandedFst<W>>(fst: &F) {
    for s in 0..fst.num_states() {
        println!("s = {} num_arcs = {}", s, fst.num_trs(s).unwrap());
        for tr in fst.get_trs(s).unwrap().trs() {
            println!("{} {:?}", tr.ilabel, &tr.weight);
        }
    }
}

/// In place minimization for weighted final state acceptor.
/// If `allow_acyclic_minimization` is true and the input is acyclic, then a specific
/// minimization is applied.
///
/// An error is returned if the input fst is not a weighted acceptor.
pub fn acceptor_minimize<W: Semiring, F: MutableFst<W> + ExpandedFst<W>>(
    ifst: &mut F,
    allow_acyclic_minimization: bool,
) -> Result<()> {
    // for s in 1..160 {
    //     println!("[AcceptorMinimize] s = {} num_arcs = {}", s, ifst.num_trs(s).unwrap());
    //     for tr in ifst.get_trs(s).unwrap().trs() {
    //         print!("{} ", tr.ilabel);
    //     }
    //     print!("\n");
    // }

    let props = ifst.compute_and_update_properties(
        FstProperties::ACCEPTOR | FstProperties::UNWEIGHTED | FstProperties::ACYCLIC,
    )?;
    if !props.contains(FstProperties::ACCEPTOR | FstProperties::UNWEIGHTED) {
        bail!("FST is not an unweighted acceptor");
    }

    connect(ifst)?;

    if ifst.num_states() == 0 {
        return Ok(());
    }

    if allow_acyclic_minimization && props.contains(FstProperties::ACYCLIC) {
        // Acyclic minimization
        println!("Acyclic Minimize");
        tr_sort(ifst, ILabelCompare {});
        let minimizer = AcyclicMinimizer::new(ifst)?;
        merge_states(minimizer.get_partition(), ifst)?;
    } else {
        println!("Cyclic Minimize");
        let p = cyclic_minimize(ifst)?;
        println!("Merge states");
        merge_states(p, ifst)?;
    }

    println!("TrUnique");
    tr_unique(ifst);

    Ok(())
}

fn merge_states<W: Semiring, F: MutableFst<W>>(partition: Partition, fst: &mut F) -> Result<()> {
    let mut state_map = vec![None; partition.num_classes()];
    println!("num_states = {}", fst.num_states());
    dbg!("Part 1");
    for (i, s) in state_map
        .iter_mut()
        .enumerate()
        .take(partition.num_classes())
    {
        *s = partition.iter(i).next();
    }

    dbg!("Part 2");
    dbg!(partition.num_classes());

    for c in 0..partition.num_classes() {
        for s in partition.iter(c) {
            if s == state_map[c].unwrap() {
                let mut it_tr = fst.tr_iter_mut(s)?;
                for idx_tr in 0..it_tr.len() {
                    let tr = unsafe { it_tr.get_unchecked(idx_tr) };
                    let nextstate = state_map[partition.get_class_id(tr.nextstate)].unwrap();
                    unsafe { it_tr.set_nextstate_unchecked(idx_tr, nextstate) };
                }
            } else {
                let trs: Vec<_> = fst
                    .get_trs(s)?
                    .trs()
                    .iter()
                    .cloned()
                    .map(|mut tr| {
                        tr.nextstate = state_map[partition.get_class_id(tr.nextstate)].unwrap();
                        tr
                    })
                    .collect();
                for tr in trs.into_iter() {
                    fst.add_tr(state_map[c].unwrap(), tr)?;
                }
            }
        }
    }

    fst.set_start(state_map[partition.get_class_id(fst.start().unwrap())].unwrap())?;
    dbg!("connect");
    connect(fst)?;
    dbg!("connected");
    Ok(())
}

// Compute the height (distance) to final state
pub fn fst_depth<W: Semiring, F: Fst<W>>(
    fst: &F,
    state_id_cour: StateId,
    accessible_states: &mut HashSet<StateId>,
    fully_examined_states: &mut HashSet<StateId>,
    heights: &mut Vec<i32>,
) -> Result<()> {
    accessible_states.insert(state_id_cour);

    for _ in heights.len()..=state_id_cour {
        heights.push(-1);
    }

    let mut height_cur_state = 0;
    for tr in fst.get_trs(state_id_cour)?.trs() {
        let nextstate = tr.nextstate;

        if !accessible_states.contains(&nextstate) {
            fst_depth(
                fst,
                nextstate,
                accessible_states,
                fully_examined_states,
                heights,
            )?;
        }

        height_cur_state = max(height_cur_state, 1 + heights[nextstate]);
    }
    fully_examined_states.insert(state_id_cour);

    heights[state_id_cour] = height_cur_state;

    Ok(())
}

struct AcyclicMinimizer {
    partition: Partition,
}

impl AcyclicMinimizer {
    pub fn new<W: Semiring, F: MutableFst<W>>(fst: &mut F) -> Result<Self> {
        let mut c = Self {
            partition: Partition::empty_new(),
        };
        c.initialize(fst)?;
        c.refine(fst);
        Ok(c)
    }

    fn initialize<W: Semiring, F: MutableFst<W>>(&mut self, fst: &mut F) -> Result<()> {
        let mut accessible_state = HashSet::new();
        let mut fully_examined_states = HashSet::new();
        let mut heights = Vec::new();
        fst_depth(
            fst,
            fst.start().unwrap(),
            &mut accessible_state,
            &mut fully_examined_states,
            &mut heights,
        )?;
        self.partition.initialize(heights.len());
        self.partition
            .allocate_classes((heights.iter().max().unwrap() + 1) as usize);
        for (s, h) in heights.iter().enumerate() {
            self.partition.add(s, *h as usize);
        }
        Ok(())
    }

    fn refine<W: Semiring, F: MutableFst<W>>(&mut self, fst: &mut F) {
        let state_cmp = StateComparator {
            fst,
            // This clone is necessary for the moment because the partition is modified while
            // still needing the StateComparator.
            // TODO: Find a way to remove the clone.
            partition: self.partition.clone(),
            w: PhantomData,
        };

        let height = self.partition.num_classes();
        for h in 0..height {
            // We need here a binary search tree in order to order the states id and create a partition.
            // For now uses the crate `stable_bst` which is quite old but seems to do the job
            // TODO: Bench the performances of the implementation. Maybe re-write it.
            let mut equiv_classes =
                TreeMap::<StateId, StateId, _>::with_comparator(|a: &usize, b: &usize| {
                    state_cmp.compare(*a, *b).unwrap()
                });

            let it_partition: Vec<_> = self.partition.iter(h).collect();
            equiv_classes.insert(it_partition[0], h);

            let mut classes_to_add = vec![];
            for e in it_partition.iter().skip(1) {
                // TODO: Remove double lookup
                if equiv_classes.contains_key(e) {
                    equiv_classes.insert(*e, NO_STATE_ID);
                } else {
                    classes_to_add.push(e);
                    equiv_classes.insert(*e, NO_STATE_ID);
                }
            }

            for v in classes_to_add {
                equiv_classes.insert(*v, self.partition.add_class());
            }

            for s in it_partition {
                let old_class = self.partition.get_class_id(s);
                let new_class = *equiv_classes.get(&s).unwrap();
                if new_class == NO_STATE_ID {
                    // The behaviour here is a bit different compared to the c++ because here
                    // when inserting an equivalent key it modifies the key
                    // which is not the case in c++.
                    continue;
                }
                if old_class != (new_class as usize) {
                    self.partition.move_element(s, new_class as usize);
                }
            }
        }
    }

    pub fn get_partition(self) -> Partition {
        self.partition
    }
}

struct StateComparator<'a, W: Semiring, F: MutableFst<W>> {
    fst: &'a F,
    partition: Partition,
    w: PhantomData<W>,
}

impl<'a, W: Semiring, F: MutableFst<W>> StateComparator<'a, W, F> {
    fn do_compare(&self, x: StateId, y: StateId) -> Result<bool> {
        let xfinal = self.fst.final_weight(x)?.unwrap_or_else(W::zero);
        let yfinal = self.fst.final_weight(y)?.unwrap_or_else(W::zero);

        if xfinal < yfinal {
            return Ok(true);
        } else if xfinal > yfinal {
            return Ok(false);
        }

        if self.fst.num_trs(x)? < self.fst.num_trs(y)? {
            return Ok(true);
        }
        if self.fst.num_trs(x)? > self.fst.num_trs(y)? {
            return Ok(false);
        }

        let it_x_owner = self.fst.get_trs(x)?;
        let it_x = it_x_owner.trs().iter();
        let it_y_owner = self.fst.get_trs(y)?;
        let it_y = it_y_owner.trs().iter();

        for (arc1, arc2) in it_x.zip(it_y) {
            if arc1.ilabel < arc2.ilabel {
                return Ok(true);
            }
            if arc1.ilabel > arc2.ilabel {
                return Ok(false);
            }
            let id_1 = self.partition.get_class_id(arc1.nextstate);
            let id_2 = self.partition.get_class_id(arc2.nextstate);
            if id_1 < id_2 {
                return Ok(true);
            }
            if id_1 > id_2 {
                return Ok(false);
            }
        }
        Ok(false)
    }

    pub fn compare(&self, x: StateId, y: StateId) -> Result<Ordering> {
        if x == y {
            return Ok(Ordering::Equal);
        }

        let x_y = self.do_compare(x, y).unwrap();
        let y_x = self.do_compare(y, x).unwrap();

        if !(x_y) && !(y_x) {
            return Ok(Ordering::Equal);
        }

        if x_y {
            Ok(Ordering::Less)
        } else {
            Ok(Ordering::Greater)
        }
    }
}

struct StateIlabelHasher<'a, W: Semiring, F: Fst<W>> {
    fst: &'a F,
    ghost: PhantomData<W>,
}

impl<'a, W: Semiring, F: Fst<W>> StateIlabelHasher<'a, W, F> {
    pub fn new(fst: &'a F) -> Self {
        Self {
            fst,
            ghost: PhantomData,
        }
    }

    pub fn get_hash(&self, state: StateId) -> usize {
        let p1 = 7603;
        let p2 = 433024223;
        let mut result = p2;
        let mut current_ilabel = std::usize::MAX;
        for tr in self.fst.get_trs(state).unwrap().trs() {
            let this_label = tr.ilabel;
            if this_label != current_ilabel {
                result = p1 * result + this_label;
                current_ilabel = this_label;
            }
        }
        return result;
    }
}

fn pre_partition<W: Semiring, F: MutableFst<W>>(
    fst: &F,
    partition: &mut Partition,
    queue: &mut LifoQueue,
) {
    let mut next_class: StateId = 0;
    let num_states = fst.num_states();
    println!("[PrePartition] num_states = {}", num_states);
    let mut state_to_initial_class: Vec<StateId> = vec![0; num_states];
    {
        let hasher = StateIlabelHasher::new(fst);
        let mut hash_to_class_nonfinal = HashMap::<usize, StateId>::new();
        let mut hash_to_class_final = HashMap::<usize, StateId>::new();

        // for s in 1..160 {
        //     println!("[PrePartition] s = {} num_arcs = {} fw = {:?} hasher = {}", s, fst.num_trs(s).unwrap(), fst.final_weight(s).unwrap(), hasher.get_hash(s));
        //     for tr in fst.get_trs(s).unwrap().trs() {
        //         print!("{} ", tr.ilabel);
        //     }
        //     print!("\n");
        // }

        for (s, state_to_initial_class_s) in state_to_initial_class
            .iter_mut()
            .enumerate()
            .take(num_states)
        {
            let zero = W::zero();
            let final_weight = fst.final_weight(s).unwrap();
            if final_weight == Some(zero) {
                panic!("oups")
            }

            let this_map = if unsafe { fst.is_final_unchecked(s) } {
                &mut hash_to_class_final
            } else {
                &mut hash_to_class_nonfinal
            };

            match this_map.entry(hasher.get_hash(s)) {
                Entry::Occupied(e) => {
                    *state_to_initial_class_s = *e.get();
                }
                Entry::Vacant(e) => {
                    e.insert(next_class);
                    *state_to_initial_class_s = next_class;
                    next_class += 1;
                }
            };

            // println!("s = {} next_class = {}", s, next_class);
        }
    }

    println!("[Prepartition] next_class = {}", next_class);

    partition.allocate_classes(next_class);
    for (s, c) in state_to_initial_class.iter().enumerate().take(num_states) {
        partition.add(s, *c);
    }

    for c in 0..next_class {
        queue.enqueue(c);
    }
}

fn cyclic_minimize<W: Semiring, F: MutableFst<W>>(fst: &mut F) -> Result<Partition> {
    // Initialize
    println!("[Cyclic Minimize] Initialize");
    let mut tr: VectorFst<W::ReverseWeight> = reverse(fst)?;
    tr_sort(&mut tr, ILabelCompare {});
    println!("[Cyclic Minimize] tr.num_states() = {}", tr.num_states());
    let mut partition = Partition::new(tr.num_states() - 1);
    let mut queue = LifoQueue::default();
    pre_partition(fst, &mut partition, &mut queue);

    // Compute
    println!("[Cyclic Minimize] Compute");
    println!("[Cyclic Minimize] queue.head() = {:?}", queue.head());
    while !queue.is_empty() {
        let c = queue.head().unwrap();
        queue.dequeue();

        // Split
        // println!("[Cyclic Minimize] Split on C = {}", c);
        // TODO: Avoid this clone :o
        // Here we need to pointer to the partition that is valid even if the partition changes.
        let comp = TrIterCompare {
            partition: partition.clone(),
        };
        let mut aiter_queue = BinaryHeap::new_by(|v1, v2| {
            if comp.compare(v1, v2) {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        });

        // Split
        for s in partition.iter(c) {
            if tr.num_trs(s + 1)? > 0 {
                aiter_queue.push(TrsIterCollected {
                    idx: 0,
                    trs: tr.get_trs(s + 1)?,
                    w: PhantomData,
                });
            }
        }

        let mut prev_label = -1;
        while !aiter_queue.is_empty() {
            let mut aiter = aiter_queue.pop().unwrap();
            if aiter.done() {
                continue;
            }
            let tr = aiter.peek().unwrap();
            let from_state = tr.nextstate - 1;
            let from_label = tr.ilabel;
            if prev_label != from_label as i32 {
                partition.finalize_split(&mut Some(&mut queue));
            }
            let from_class = partition.get_class_id(from_state);
            if partition.get_class_size(from_class) > 1 {
                partition.split_on(from_state);
            }
            prev_label = from_label as i32;
            aiter.next();
            if !aiter.done() {
                aiter_queue.push(aiter);
            }
        }

        partition.finalize_split(&mut Some(&mut queue));
    }

    // Get Partition
    Ok(partition)
}

struct TrsIterCollected<W: Semiring, T: Trs<W>> {
    idx: usize,
    trs: T,
    w: PhantomData<W>,
}

impl<W: Semiring, T: Trs<W>> TrsIterCollected<W, T> {
    fn peek(&self) -> Option<&Tr<W>> {
        self.trs.trs().get(self.idx)
    }

    fn done(&self) -> bool {
        self.idx >= self.trs.len()
    }

    fn next(&mut self) {
        self.idx += 1;
    }
}

#[derive(Clone)]
struct TrIterCompare {
    partition: Partition,
}

impl TrIterCompare {
    fn compare<W: Semiring, T: Trs<W>>(
        &self,
        x: &TrsIterCollected<W, T>,
        y: &TrsIterCollected<W, T>,
    ) -> bool
    where
        W: Semiring,
    {
        let xarc = x.peek().unwrap();
        let yarc = y.peek().unwrap();
        xarc.ilabel > yarc.ilabel
    }
}