use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::primitives::{Address, B256, Bytes, Delta, EMPTY_CODE_HASH, U256};
use crate::trace::{AccountState, PrestateDiff, TxTrace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Change<T> {
    pub from: T,
    pub to: T,
}

impl<T: PartialEq> Change<T> {
    fn new(from: T, to: T) -> Option<Self> {
        (from != to).then_some(Self { from, to })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    /// The account did not exist (was empty) before.
    Created,
    /// The account was removed (self-destructed).
    Destroyed,
    Modified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AccountDiff {
    pub status: AccountStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<Change<U256>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<Change<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Change<Bytes>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_hash: Option<Change<B256>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub storage: BTreeMap<B256, Change<B256>>,
}

impl AccountDiff {
    fn empty(status: AccountStatus) -> Self {
        Self { status, balance: None, nonce: None, code: None, code_hash: None, storage: BTreeMap::new() }
    }

    pub fn has_changes(&self) -> bool {
        self.balance.is_some()
            || self.nonce.is_some()
            || self.code.is_some()
            || self.code_hash.is_some()
            || !self.storage.is_empty()
    }

    pub fn balance_delta(&self) -> Option<Delta> {
        self.balance.map(|c| Delta::between(c.from, c.to))
    }
}

/// Net state changes, keyed by account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StateDiff {
    pub accounts: BTreeMap<Address, AccountDiff>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub accounts: usize,
    pub created: usize,
    pub destroyed: usize,
    pub balance_changes: usize,
    pub nonce_changes: usize,
    pub code_changes: usize,
    pub storage_changes: usize,
    /// Sum of all balance increases, in wei.
    pub total_credited: U256,
    /// Sum of all balance decreases, in wei.
    pub total_debited: U256,
}

fn balance_of(a: Option<&AccountState>) -> U256 {
    a.and_then(|a| a.balance).unwrap_or_default()
}

fn nonce_of(a: Option<&AccountState>) -> u64 {
    a.and_then(|a| a.nonce).unwrap_or(0)
}

fn code_of(a: Option<&AccountState>) -> Bytes {
    a.and_then(|a| a.code.clone()).unwrap_or_default()
}

fn code_hash_of(a: Option<&AccountState>) -> B256 {
    a.and_then(|a| a.code_hash).unwrap_or(EMPTY_CODE_HASH)
}

impl StateDiff {
    /// Reconstructs explicit before/after values from geth's diff-mode
    /// encoding:
    ///
    /// * `post` lists only changed fields, so a missing field is unchanged;
    /// * an account only in `post` was empty before (created);
    /// * an account only in `pre` was deleted (self-destructed);
    /// * a slot in `pre` but missing from a present `post` was cleared to zero;
    /// * a slot only in `post` was zero before.
    pub fn from_prestate(trace: &PrestateDiff) -> Self {
        let addresses: BTreeSet<&Address> = trace.pre.keys().chain(trace.post.keys()).collect();
        let mut accounts = BTreeMap::new();
        for addr in addresses {
            let pre = trace.pre.get(addr);
            let post = trace.post.get(addr);
            let diff = match (pre, post) {
                (Some(pre), None) => destroyed(pre),
                (None, Some(post)) => modified(AccountStatus::Created, None, post),
                (Some(pre), Some(post)) => modified(AccountStatus::Modified, Some(pre), post),
                (None, None) => unreachable!("address comes from pre or post"),
            };
            if diff.has_changes() {
                accounts.insert(*addr, diff);
            }
        }
        Self { accounts }
    }

    /// Folds per-transaction diffs, applied in order, into one net diff.
    pub fn compose<'a>(diffs: impl IntoIterator<Item = &'a StateDiff>) -> Self {
        let mut out = StateDiff::default();
        for diff in diffs {
            out = out.then(diff);
        }
        out
    }

    /// The net effect of `self` followed by `next`.
    pub fn then(&self, next: &StateDiff) -> StateDiff {
        let mut accounts = self.accounts.clone();
        for (addr, later) in &next.accounts {
            match accounts.remove(addr) {
                None => {
                    accounts.insert(*addr, later.clone());
                }
                Some(earlier) => {
                    let merged = merge(earlier, later);
                    if merged.has_changes() {
                        accounts.insert(*addr, merged);
                    }
                }
            }
        }
        StateDiff { accounts }
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary { accounts: self.accounts.len(), ..Summary::default() };
        for acct in self.accounts.values() {
            match acct.status {
                AccountStatus::Created => s.created += 1,
                AccountStatus::Destroyed => s.destroyed += 1,
                AccountStatus::Modified => {}
            }
            if let Some(delta) = acct.balance_delta() {
                s.balance_changes += 1;
                let total = if delta.negative { &mut s.total_debited } else { &mut s.total_credited };
                *total = total.checked_add(delta.magnitude).unwrap_or(U256::MAX);
            }
            s.nonce_changes += usize::from(acct.nonce.is_some());
            s.code_changes += usize::from(acct.code.is_some() || acct.code_hash.is_some());
            s.storage_changes += acct.storage.len();
        }
        s
    }
}

fn destroyed(pre: &AccountState) -> AccountDiff {
    let mut d = AccountDiff::empty(AccountStatus::Destroyed);
    d.balance = Change::new(balance_of(Some(pre)), U256::ZERO);
    d.nonce = Change::new(nonce_of(Some(pre)), 0);
    d.code = pre.code.clone().and_then(|c| Change::new(c, Bytes::default()));
    d.code_hash = pre.code_hash.and_then(|h| Change::new(h, EMPTY_CODE_HASH));
    for (slot, value) in pre.storage.iter().flatten() {
        if let Some(c) = Change::new(*value, B256::ZERO) {
            d.storage.insert(*slot, c);
        }
    }
    d
}

fn modified(status: AccountStatus, pre: Option<&AccountState>, post: &AccountState) -> AccountDiff {
    let mut d = AccountDiff::empty(status);
    d.balance = post.balance.and_then(|to| Change::new(balance_of(pre), to));
    d.nonce = post.nonce.and_then(|to| Change::new(nonce_of(pre), to));
    d.code = post.code.clone().and_then(|to| Change::new(code_of(pre), to));
    d.code_hash = post.code_hash.and_then(|to| Change::new(code_hash_of(pre), to));

    let empty = BTreeMap::new();
    let pre_slots = pre.and_then(|p| p.storage.as_ref()).unwrap_or(&empty);
    let post_slots = post.storage.as_ref().unwrap_or(&empty);
    for slot in pre_slots.keys().chain(post_slots.keys()) {
        let from = pre_slots.get(slot).copied().unwrap_or(B256::ZERO);
        let to = post_slots.get(slot).copied().unwrap_or(B256::ZERO);
        if let Some(c) = Change::new(from, to) {
            d.storage.insert(*slot, c);
        }
    }
    d
}

fn merge_field<T: Clone + PartialEq>(a: &Option<Change<T>>, b: &Option<Change<T>>) -> Option<Change<T>> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x.clone()),
        (None, Some(y)) => Some(y.clone()),
        (Some(x), Some(y)) => Change::new(x.from.clone(), y.to.clone()),
    }
}

fn merge(earlier: AccountDiff, later: &AccountDiff) -> AccountDiff {
    use AccountStatus::*;
    let status = match (earlier.status, later.status) {
        (Created, Destroyed) => Created,
        (_, Destroyed) => Destroyed,
        (Created, _) => Created,
        (Destroyed, _) => Modified,
        (Modified, s) => s,
    };
    let mut storage = earlier.storage;
    for (slot, change) in &later.storage {
        match storage.remove(slot) {
            None => {
                storage.insert(*slot, *change);
            }
            Some(first) => {
                if let Some(c) = Change::new(first.from, change.to) {
                    storage.insert(*slot, c);
                }
            }
        }
    }
    AccountDiff {
        status,
        balance: merge_field(&earlier.balance, &later.balance),
        nonce: merge_field(&earlier.nonce, &later.nonce),
        code: merge_field(&earlier.code, &later.code),
        code_hash: merge_field(&earlier.code_hash, &later.code_hash),
        storage,
    }
}

/// A transaction's diff, labelled with its hash when known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TxDiff {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<B256>,
    pub diff: StateDiff,
}

pub fn diffs_from_traces(traces: &[TxTrace]) -> Vec<TxDiff> {
    traces.iter().map(|t| TxDiff { tx_hash: t.tx_hash, diff: StateDiff::from_prestate(&t.diff) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::parse_traces_str;

    fn addr(n: u8) -> Address {
        Address([n; 20])
    }

    fn word(n: u64) -> B256 {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&n.to_be_bytes());
        B256(w)
    }

    fn diff(json: &str) -> StateDiff {
        StateDiff::from_prestate(&parse_traces_str(json).unwrap()[0].diff)
    }

    const A: &str = "0x0101010101010101010101010101010101010101";

    #[test]
    fn transfer_to_new_account() {
        let d = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x64","nonce":3}}}},
                "post":{{"{A}":{{"balance":"0x50","nonce":4}},"0x0202020202020202020202020202020202020202":{{"balance":"0xa"}}}}}}"#
        ));
        let sender = &d.accounts[&addr(1)];
        assert_eq!(sender.status, AccountStatus::Modified);
        assert_eq!(sender.balance, Some(Change { from: U256::from_u128(100), to: U256::from_u128(80) }));
        assert_eq!(sender.nonce, Some(Change { from: 3, to: 4 }));
        let receiver = &d.accounts[&addr(2)];
        assert_eq!(receiver.status, AccountStatus::Created);
        assert_eq!(receiver.balance, Some(Change { from: U256::ZERO, to: U256::from_u128(10) }));
        let s = d.summary();
        assert_eq!((s.accounts, s.created, s.balance_changes), (2, 1, 2));
        assert_eq!(s.total_credited, U256::from_u128(10));
        assert_eq!(s.total_debited, U256::from_u128(20));
    }

    #[test]
    fn storage_set_cleared_and_unchanged_fields() {
        let d = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x1","storage":{{"0x1":"0x7","0x2":"0x9"}}}}}},
                "post":{{"{A}":{{"storage":{{"0x2":"0xa","0x3":"0x5"}}}}}}}}"#
        ));
        let a = &d.accounts[&addr(1)];
        assert_eq!(a.balance, None, "balance missing from post is unchanged");
        assert_eq!(a.storage[&word(1)], Change { from: word(7), to: word(0) }, "cleared slot");
        assert_eq!(a.storage[&word(2)], Change { from: word(9), to: word(10) });
        assert_eq!(a.storage[&word(3)], Change { from: word(0), to: word(5) }, "new slot");
    }

    #[test]
    fn contract_creation_and_destruction() {
        let created = diff(&format!(
            r#"{{"pre":{{}},"post":{{"{A}":{{"nonce":1,"code":"0x6000","storage":{{"0x0":"0x1"}}}}}}}}"#
        ));
        let c = &created.accounts[&addr(1)];
        assert_eq!(c.status, AccountStatus::Created);
        assert_eq!(c.code.as_ref().unwrap().to.0, vec![0x60, 0x00]);
        assert_eq!(c.nonce, Some(Change { from: 0, to: 1 }));

        let destroyed = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x5","nonce":1,"code":"0x6000","storage":{{"0x0":"0x1","0x1":"0x0"}}}}}},"post":{{}}}}"#
        ));
        let d = &destroyed.accounts[&addr(1)];
        assert_eq!(d.status, AccountStatus::Destroyed);
        assert_eq!(d.balance.unwrap().to, U256::ZERO);
        assert_eq!(d.code.as_ref().unwrap().to, Bytes::default());
        assert_eq!(d.storage.len(), 1, "zero-valued slots are not changes");
    }

    #[test]
    fn composition_nets_out_changes() {
        let first = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x10","storage":{{"0x1":"0x1"}}}}}},"post":{{"{A}":{{"balance":"0x20","storage":{{"0x1":"0x2"}}}}}}}}"#
        ));
        let second = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x20","storage":{{"0x1":"0x2"}}}}}},"post":{{"{A}":{{"balance":"0x10","storage":{{"0x1":"0x3"}}}}}}}}"#
        ));
        let net = StateDiff::compose([&first, &second]);
        let a = &net.accounts[&addr(1)];
        assert_eq!(a.balance, None, "balance returned to its starting value");
        assert_eq!(a.storage[&word(1)], Change { from: word(1), to: word(3) });

        let back = diff(&format!(
            r#"{{"pre":{{"{A}":{{"balance":"0x10","storage":{{"0x1":"0x3"}}}}}},"post":{{"{A}":{{"storage":{{"0x1":"0x1"}}}}}}}}"#
        ));
        assert!(StateDiff::compose([&first, &second, &back]).is_empty());
    }

    #[test]
    fn created_then_destroyed_vanishes() {
        let created = diff(&format!(r#"{{"pre":{{}},"post":{{"{A}":{{"balance":"0x5"}}}}}}"#));
        let destroyed = diff(&format!(r#"{{"pre":{{"{A}":{{"balance":"0x5"}}}},"post":{{}}}}"#));
        assert!(StateDiff::compose([&created, &destroyed]).is_empty());
        let recreated = StateDiff::compose([&destroyed, &created]);
        assert!(recreated.is_empty(), "destroyed then recreated with the same balance is no net change");
    }
}
