use std::collections::BTreeMap;

use eth_state_diff::{Address, B256, Bytes, StateDiff, U256, parse_traces};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Account {
    balance: u64,
    nonce: u64,
    code: Vec<u8>,
    storage: BTreeMap<u8, u64>,
}

impl Account {
    fn is_empty(&self) -> bool {
        self.balance == 0 && self.nonce == 0 && self.code.is_empty() && self.storage.is_empty()
    }
}

type World = BTreeMap<u8, Account>;

fn addr(i: u8) -> String {
    format!("0x{}", format!("{i:02x}").repeat(20))
}

fn slot(i: u8) -> String {
    format!("0x{i:064x}")
}

fn hex(bytes: &[u8]) -> String {
    format!("0x{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn pre_json(a: &Account, slots: impl Iterator<Item = u8>) -> Value {
    let mut o = Map::new();
    o.insert("balance".into(), json!(format!("0x{:x}", a.balance)));
    if a.nonce != 0 {
        o.insert("nonce".into(), json!(a.nonce));
    }
    if !a.code.is_empty() {
        o.insert("code".into(), json!(hex(&a.code)));
    }
    let storage: Map<String, Value> =
        slots.filter_map(|s| a.storage.get(&s).map(|v| (slot(s), json!(format!("0x{v:064x}"))))).collect();
    if !storage.is_empty() {
        o.insert("storage".into(), Value::Object(storage));
    }
    Value::Object(o)
}

/// Encodes the transition `before -> after` the way geth's prestateTracer
/// does in diff mode.
fn geth_diff(before: &World, after: &World) -> Value {
    let empty = Account::default();
    let (mut pre, mut post) = (Map::new(), Map::new());
    for i in before.keys().chain(after.keys()) {
        let a0 = before.get(i).unwrap_or(&empty);
        let a1 = after.get(i).unwrap_or(&empty);
        if a0 == a1 {
            continue;
        }
        if a0.is_empty() {
            post.insert(
                addr(*i),
                pre_json(a1, a1.storage.keys().copied())
                    .as_object()
                    .map(|o| {
                        let mut o = o.clone();
                        if a1.balance == 0 {
                            o.remove("balance");
                        }
                        Value::Object(o)
                    })
                    .unwrap(),
            );
        } else if a1.is_empty() && !a0.code.is_empty() {
            pre.insert(addr(*i), pre_json(a0, a0.storage.keys().copied()));
        } else {
            let changed: Vec<u8> = a0
                .storage
                .keys()
                .chain(a1.storage.keys())
                .copied()
                .filter(|s| a0.storage.get(s) != a1.storage.get(s))
                .collect();
            pre.insert(addr(*i), pre_json(a0, changed.iter().copied()));
            let mut o = Map::new();
            if a0.balance != a1.balance {
                o.insert("balance".into(), json!(format!("0x{:x}", a1.balance)));
            }
            if a0.nonce != a1.nonce {
                o.insert("nonce".into(), json!(a1.nonce));
            }
            if a0.code != a1.code {
                o.insert("code".into(), json!(hex(&a1.code)));
            }
            let storage: Map<String, Value> = changed
                .iter()
                .filter_map(|s| a1.storage.get(s).map(|v| (slot(*s), json!(format!("0x{v:064x}")))))
                .collect();
            if !storage.is_empty() {
                o.insert("storage".into(), Value::Object(storage));
            }
            post.insert(addr(*i), Value::Object(o));
        }
    }
    json!({"pre": pre, "post": post})
}

fn decode(v: Value) -> StateDiff {
    StateDiff::from_prestate(&parse_traces(v).unwrap()[0].diff)
}

fn u64_of(v: U256) -> u64 {
    v.to_string().parse().unwrap()
}

fn word_u64(w: &B256) -> u64 {
    u64::from_be_bytes(w.0[24..].try_into().unwrap())
}

/// Applies a diff to a world, checking every `from` value along the way.
fn apply(world: &World, diff: &StateDiff) -> Result<World, String> {
    let mut out = world.clone();
    for (address, d) in &diff.accounts {
        let i = address.0[0];
        if *address != Address([i; 20]) {
            return Err(format!("unexpected address {address}"));
        }
        let acct = out.entry(i).or_default();
        if let Some(c) = d.balance {
            if u64_of(c.from) != acct.balance {
                return Err(format!("{address}: balance from {} != {}", c.from, acct.balance));
            }
            acct.balance = u64_of(c.to);
        }
        if let Some(c) = d.nonce {
            if c.from != acct.nonce {
                return Err(format!("{address}: nonce from {} != {}", c.from, acct.nonce));
            }
            acct.nonce = c.to;
        }
        if let Some(c) = &d.code {
            if c.from != Bytes(acct.code.clone()) {
                return Err(format!("{address}: code from mismatch"));
            }
            acct.code = c.to.0.clone();
        }
        for (s, c) in &d.storage {
            let key = s.0[31];
            if word_u64(&c.from) != acct.storage.get(&key).copied().unwrap_or(0) {
                return Err(format!("{address}: slot {key} from mismatch"));
            }
            match word_u64(&c.to) {
                0 => acct.storage.remove(&key),
                v => acct.storage.insert(key, v),
            };
        }
    }
    out.retain(|_, a| !a.is_empty());
    Ok(out)
}

fn account() -> impl Strategy<Value = Account> {
    (
        prop_oneof![Just(0u64), 1u64..1_000_000],
        prop_oneof![Just(0u64), 1u64..10],
        prop_oneof![Just(vec![]), prop::collection::vec(any::<u8>(), 1..4)],
        prop::collection::btree_map(0u8..4, 1u64..100, 0..3),
    )
        .prop_map(|(balance, nonce, code, storage)| {
            // Only contracts hold storage.
            let storage = if code.is_empty() { BTreeMap::new() } else { storage };
            Account { balance, nonce, code, storage }
        })
}

fn world() -> impl Strategy<Value = World> {
    prop::collection::btree_map(1u8..6, account(), 0..5).prop_map(|mut w| {
        w.retain(|_, a| !a.is_empty());
        w
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn decoded_diff_reproduces_the_transition(before in world(), after in world()) {
        let diff = decode(geth_diff(&before, &after));
        prop_assert_eq!(apply(&before, &diff), Ok(after));
    }

    #[test]
    fn composition_equals_end_to_end(states in prop::collection::vec(world(), 2..6)) {
        let diffs: Vec<StateDiff> = states.windows(2).map(|w| decode(geth_diff(&w[0], &w[1]))).collect();
        let net = StateDiff::compose(&diffs);
        let first = states.first().unwrap();
        let last = states.last().unwrap();
        prop_assert_eq!(apply(first, &net), Ok(last.clone()));

        let direct = decode(geth_diff(first, last));
        let strip = |d: &StateDiff| -> Vec<_> {
            d.accounts.iter().map(|(a, x)| (*a, x.balance, x.nonce, x.code.clone(), x.storage.clone())).collect()
        };
        prop_assert_eq!(strip(&net), strip(&direct));
    }
}
